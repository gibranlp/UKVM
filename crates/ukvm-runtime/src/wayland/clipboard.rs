use super::*;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{mpsc, watch},
};
use ukvm_network::{clipboard as wire, trust::SessionPermission};
use ukvm_platform::clipboard::{ClipboardBackend, ClipboardEvent, ClipboardWrite};
use ukvm_protocol::clipboard::ClipboardMessage;

pub use crate::snapshot::ClipboardStatus;
use ukvm_core::clipboard::ClipboardState as Engine;

pub(super) struct Clipboard {
    engine: Mutex<Engine>,
    latest: watch::Sender<Option<ClipboardMessage>>,
    writes: watch::Sender<Option<ClipboardWrite>>,
    status: Mutex<ClipboardStatus>,
    connected: Mutex<std::collections::HashSet<String>>,
}
impl Clipboard {
    pub(super) fn new(id: &str) -> Result<Self, String> {
        Ok(Self {
            engine: Mutex::new(Engine::new(id, wire::epoch()?)),
            latest: watch::channel(None).0,
            writes: watch::channel(None).0,
            status: Mutex::new(ClipboardStatus {
                state: "disabled".into(),
                ..Default::default()
            }),
            connected: Mutex::new(Default::default()),
        })
    }
    pub(super) fn snapshot(&self) -> ClipboardStatus {
        let mut state = self.status.lock().unwrap().clone();
        if state.connected_peers > 0 {
            state.state = "sharing".into();
        }
        state
    }
    fn sync(&self) {
        self.status.lock().unwrap().last_sync = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs());
    }
}
fn permitted(context: &Context, peer: &str) -> bool {
    TrustStore::load(&context.peers.trust_dir).is_ok_and(|store| {
        store
            .authorize(peer, SessionPermission::ShareClipboard)
            .is_ok()
    })
}
fn ready(context: &Context) -> bool {
    context.clipboard.status.lock().unwrap().supported == Some(true)
}

type BackendTask = std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>>>>;

pub(super) async fn supervise(context: &Context, mut stop: watch::Receiver<bool>) {
    let (changes, mut events) = watch::channel(None);
    let (backend_stop, backend_rx) = watch::channel(false);
    let mut backend: Option<BackendTask> = None;
    let mut workers = FuturesUnordered::new();
    let mut active = std::collections::HashSet::new();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut retry = Instant::now();
    loop {
        tokio::select! {
            _ = cancelled(&mut stop) => break,
            result = workers.next(), if !workers.is_empty() => { if let Some(id) = result { active.remove(&id); } },
            result = async { backend.as_mut().unwrap().await }, if backend.is_some() => {
                backend = None;
                let mut state = context.clipboard.status.lock().unwrap();
                state.supported = Some(false); state.state = "unavailable".into(); state.error = result.err();
                retry = Instant::now()+Duration::from_secs(10);
            },
            result = events.changed(), if backend.is_some() => {
                if result.is_ok() {
                    let event = events.borrow_and_update().clone();
                    let enabled = TrustStore::load(&context.peers.trust_dir).is_ok_and(|store| store.peers.iter().any(|p| p.permissions.connect && p.permissions.share_clipboard));
                    if enabled {
                        { let mut state = context.clipboard.status.lock().unwrap(); state.supported = Some(true); state.state = "ready".into(); }
                        match event {
                            Some(ClipboardEvent::Text(text)) => {
                                if let Some(message) = context.clipboard.engine.lock().unwrap().local(text) { context.clipboard.latest.send_replace(Some(message)); }
                            },
                            Some(ClipboardEvent::Rejected(error)) => { context.clipboard.status.lock().unwrap().error = Some(error.into()); context.clipboard.engine.lock().unwrap().clear(); context.clipboard.latest.send_replace(None); },
                            Some(ClipboardEvent::Ignored) => { context.clipboard.engine.lock().unwrap().clear(); context.clipboard.latest.send_replace(None); },
                            Some(ClipboardEvent::Ready) => { let mut state = context.clipboard.status.lock().unwrap(); state.supported = Some(true); state.state = "ready".into(); },
                            None => {},
                        }
                    }
                }
            },
            _ = tick.tick() => {
                let store = TrustStore::load(&context.peers.trust_dir).unwrap_or_default();
                let enabled = store.peers.iter().any(|p| p.permissions.connect && p.permissions.share_clipboard);
                context.clipboard.status.lock().unwrap().enabled = enabled;
                if !enabled {
                    if let Some(mut task) = backend.take() { backend_stop.send_replace(true); let _ = (&mut task).await; }
                    context.clipboard.latest.send_replace(None); context.clipboard.writes.send_replace(None);
                    context.clipboard.engine.lock().unwrap().clear();
                    let mut state = context.clipboard.status.lock().unwrap(); state.state = "disabled".into(); state.error = None;
                    continue;
                }
                if backend.is_none() && Instant::now() >= retry {
                    backend_stop.send_replace(false);
                    context.clipboard.writes.send_replace(None);
                    let future = ukvm_platform::clipboard::wayland::WaylandClipboard::default().run(changes.clone(),context.clipboard.writes.subscribe(),backend_rx.clone());
                    backend = Some(Box::pin(future));
                    let mut state = context.clipboard.status.lock().unwrap(); state.supported = None; state.state = "starting".into(); state.error = None;
                }
                if ready(context) {
                    for peer in store.peers.iter().filter(|p| p.permissions.connect && p.permissions.share_clipboard && context.peers.identity.id < p.id).take(64) {
                        if active.insert(peer.id.clone()) { workers.push(peer_worker(context,peer.id.clone(),stop.clone())); }
                    }
                }
            }
        }
    }
    backend_stop.send_replace(true);
    if let Some(task) = backend {
        let _ = task.await;
    }
    while workers.next().await.is_some() {}
    context.clipboard.latest.send_replace(None);
    context.clipboard.writes.send_replace(None);
}

async fn peer_worker(context: &Context, id: String, mut stop: watch::Receiver<bool>) -> String {
    let mut backoff = Backoff::default();
    while !*stop.borrow() && permitted(context, &id) && ready(context) {
        let peer = TrustStore::load(&context.peers.trust_dir)
            .ok()
            .and_then(|store| store.find_id(&id).cloned());
        let Some(peer) = peer else {
            break;
        };
        let mut connected = false;
        for address in addresses(&context.peers, &peer) {
            let certificate = cert_path(&context.peers.trust_dir, &id);
            let connection = tokio::select! { _ = cancelled(&mut stop) => return id, result = wire::connect(&address,&context.peers.identity,&certificate,&id) => result };
            if let Ok(stream) = connection {
                connected = true;
                backoff.reset();
                if let Err(error) = session(context, &id, stream, stop.clone()).await {
                    context.peers.log(format!("clipboard peer {id}: {error}"));
                    context.clipboard.status.lock().unwrap().error = Some(error);
                }
                break;
            }
        }
        if !connected {
            context.clipboard.status.lock().unwrap().error = Some("Clipboard peer unavailable or unsupported; both nodes need M19 and clipboard permission".into());
        }
        pause(backoff.failed(), &mut stop).await;
    }
    id
}

pub(super) async fn session<S: AsyncRead + AsyncWrite + Unpin>(
    context: &Context,
    id: &str,
    mut stream: S,
    mut stop: watch::Receiver<bool>,
) -> Result<(), String> {
    if !permitted(context, id) || !ready(context) {
        return Err("clipboard sharing denied or backend unavailable".into());
    }
    tokio::select! { _ = cancelled(&mut stop) => return Ok(()), result = wire::negotiate(&mut stream) => result? }
    if !permitted(context, id) {
        return Err("clipboard permission revoked".into());
    }
    {
        let mut connected = context.clipboard.connected.lock().unwrap();
        if connected.len() >= 64 || !connected.insert(id.into()) {
            return Err("duplicate or excess clipboard connection".into());
        }
    }
    context.clipboard.status.lock().unwrap().connected_peers =
        context.clipboard.connected.lock().unwrap().len();
    context.clipboard.status.lock().unwrap().error = None;
    let (mut reader, mut writer) = tokio::io::split(stream);
    let (messages, mut receive) = mpsc::channel(4);
    let (reader_stop, mut reader_stop_rx) = watch::channel(false);
    let reading = async {
        loop {
            let message = tokio::select! { _ = cancelled(&mut reader_stop_rx) => break, result = wire::read(&mut reader) => result };
            let failed = message.is_err();
            tokio::select! { _ = cancelled(&mut reader_stop_rx) => break, _ = messages.send(message) => {} }
            if failed {
                break;
            }
        }
    };
    let writing = async {
        let result = async {
            let mut latest = context.clipboard.latest.subscribe();
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            let mut last_receive = Instant::now();
            let mut rate_start = Instant::now(); let mut rate = 0;
            let mut initial = true;
            loop {
                if initial {
                    initial = false;
                    let message = latest.borrow_and_update().clone();
                    if let Some(message) = message { send(context,id,&mut writer,&message).await?; }
                }
                tokio::select! {
                    _ = cancelled(&mut stop) => break,
                    result = latest.changed() => {
                        if result.is_err() { break; }
                        let message = latest.borrow_and_update().clone();
                        if let Some(message) = message { send(context,id,&mut writer,&message).await?; }
                    },
                    message = receive.recv() => {
                        let message = message.ok_or("clipboard channel closed")??;
                        last_receive = Instant::now();
                        if !permitted(context,id) || !ready(context) { return Err("clipboard permission revoked or backend unavailable".into()); }
                        if rate_start.elapsed() >= Duration::from_secs(1) { rate_start = Instant::now(); rate = 0; }
                        rate += 1; if rate > 32 { return Err("clipboard rate limit exceeded".into()); }
                        match message {
                            ClipboardMessage::Text {..} => {
                                let text = context.clipboard.engine.lock().unwrap().remote(id,&message)?;
                                if let Some(text) = text {
                                    context.clipboard.latest.send_replace(None);
                                    let trust = context.peers.trust_dir.clone(); let peer = id.to_string();
                                    context.clipboard.writes.send_replace(Some(ClipboardWrite { text, authorize: Arc::new(move || ukvm_network::trust::clipboard_guard(&trust,&peer).ok().map(|guard| Box::new(guard) as Box<dyn Send>)) })); context.clipboard.sync();
                                }
                            },
                            ClipboardMessage::Ping => wire::write(&mut writer,&ClipboardMessage::Pong).await?,
                            ClipboardMessage::Pong => {},
                            _ => return Err("unexpected clipboard message".into()),
                        }
                    },
                    _ = tick.tick() => {
                        if !permitted(context,id) || !ready(context) { return Err("clipboard permission revoked or backend unavailable".into()); }
                        if last_receive.elapsed() > Duration::from_secs(12) { return Err("clipboard peer heartbeat timed out".into()); }
                        wire::write(&mut writer,&ClipboardMessage::Ping).await?;
                    }
                }
            }
            Ok(())
        }.await;
        reader_stop.send_replace(true);
        result
    };
    let ((), result) = tokio::join!(reading, writing);
    context.clipboard.connected.lock().unwrap().remove(id);
    context.clipboard.status.lock().unwrap().connected_peers =
        context.clipboard.connected.lock().unwrap().len();
    result
}

async fn send<W: AsyncWrite + Unpin>(
    context: &Context,
    id: &str,
    writer: &mut W,
    message: &ClipboardMessage,
) -> Result<(), String> {
    let _guard = ukvm_network::trust::clipboard_guard(&context.peers.trust_dir, id)?;
    if let ClipboardMessage::Text { text, .. } = message {
        if !context.clipboard.engine.lock().unwrap().matches(text) {
            return Ok(());
        }
    }
    wire::write(writer, message).await?;
    context.clipboard.sync();
    Ok(())
}

#[cfg(test)]
#[path = "clipboard_tests.rs"]
mod integration_tests;
