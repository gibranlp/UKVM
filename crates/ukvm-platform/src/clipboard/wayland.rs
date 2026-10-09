use super::{ClipboardBackend, ClipboardEvent, ClipboardWrite};
use futures_util::{stream::FuturesUnordered, StreamExt};
use std::{
    collections::{HashMap, VecDeque},
    os::fd::{AsFd, OwnedFd},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{unix::AsyncFd, AsyncReadExt},
    sync::watch,
};
use ukvm_protocol::clipboard::MAX_CLIPBOARD_BYTES;
use wayland_client::{
    protocol::{wl_callback::WlCallback, wl_registry, wl_seat::WlSeat},
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols::ext::data_control::v1::client::{
    ext_data_control_device_v1::{self, ExtDataControlDeviceV1 as ExtDevice},
    ext_data_control_manager_v1::ExtDataControlManagerV1 as ExtManager,
    ext_data_control_offer_v1::{self, ExtDataControlOfferV1 as ExtOffer},
    ext_data_control_source_v1::{self, ExtDataControlSourceV1 as ExtSource},
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::{self, ZwlrDataControlDeviceV1 as WlrDevice},
    zwlr_data_control_manager_v1::ZwlrDataControlManagerV1 as WlrManager,
    zwlr_data_control_offer_v1::{self, ZwlrDataControlOfferV1 as WlrOffer},
    zwlr_data_control_source_v1::{self, ZwlrDataControlSourceV1 as WlrSource},
};

const TEXT_MIMES: [&str; 3] = ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING"];
#[derive(Clone)]
enum Manager {
    Ext(ExtManager),
    Wlr(WlrManager),
}
enum Device {
    Ext(ExtDevice),
    Wlr(WlrDevice),
}
#[derive(Clone, PartialEq, Eq, Hash)]
enum Offer {
    Ext(ExtOffer),
    Wlr(WlrOffer),
}
#[derive(Clone, PartialEq, Eq)]
enum Source {
    Ext(ExtSource),
    Wlr(WlrSource),
}
impl Manager {
    fn get_data_device(&self, seat: &WlSeat, qh: &QueueHandle<State>, _: ()) -> Device {
        match self {
            Self::Ext(manager) => Device::Ext(manager.get_data_device(seat, qh, ())),
            Self::Wlr(manager) => Device::Wlr(manager.get_data_device(seat, qh, ())),
        }
    }
    fn create_data_source(&self, qh: &QueueHandle<State>, _: ()) -> Source {
        match self {
            Self::Ext(manager) => Source::Ext(manager.create_data_source(qh, ())),
            Self::Wlr(manager) => Source::Wlr(manager.create_data_source(qh, ())),
        }
    }
}
impl Device {
    fn destroy(&self) {
        match self {
            Self::Ext(device) => device.destroy(),
            Self::Wlr(device) => device.destroy(),
        }
    }
    fn set_selection(&self, source: Option<&Source>) {
        match (self, source) {
            (Self::Ext(device), Some(Source::Ext(source))) => device.set_selection(Some(source)),
            (Self::Wlr(device), Some(Source::Wlr(source))) => device.set_selection(Some(source)),
            _ => {}
        }
    }
}
impl Offer {
    fn destroy(&self) {
        match self {
            Self::Ext(offer) => offer.destroy(),
            Self::Wlr(offer) => offer.destroy(),
        }
    }
    fn receive(&self, mime: String, fd: std::os::fd::BorrowedFd<'_>) {
        match self {
            Self::Ext(offer) => offer.receive(mime, fd),
            Self::Wlr(offer) => offer.receive(mime, fd),
        }
    }
}
impl Source {
    fn destroy(&self) {
        match self {
            Self::Ext(source) => source.destroy(),
            Self::Wlr(source) => source.destroy(),
        }
    }
    fn offer(&self, mime: String) {
        match self {
            Self::Ext(source) => source.offer(mime),
            Self::Wlr(source) => source.offer(mime),
        }
    }
}
enum Work {
    Read(Offer, String),
    Write(OwnedFd, Arc<String>),
    Clear,
    Ignored,
    Owned,
}
struct State {
    manager: Option<Manager>,
    seat: Option<WlSeat>,
    offers: HashMap<Offer, Vec<String>>,
    source: Option<(Source, Arc<String>)>,
    work: VecDeque<Work>,
    finished: bool,
    registry_ready: bool,
    owned_mime: String,
}
impl State {
    fn selection(&mut self, offer: Option<Offer>) {
        if let Some(offer) = offer {
            let mimes = self.offers.remove(&offer).unwrap_or_default();
            if mimes.contains(&self.owned_mime) {
                offer.destroy();
                self.work.push_back(Work::Owned);
                return;
            }
            if let Some(mime) = TEXT_MIMES
                .iter()
                .find(|mime| mimes.iter().any(|m| m == **mime))
            {
                self.work.push_back(Work::Read(offer, (*mime).into()));
            } else {
                offer.destroy();
                self.work.push_back(Work::Ignored);
            }
        } else {
            self.work.push_back(Work::Clear);
        }
    }
}
impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "ext_data_control_manager_v1" => {
                    state.manager = Some(Manager::Ext(registry.bind::<ExtManager, _, _>(
                        name,
                        version.min(1),
                        qh,
                        (),
                    )))
                }
                "zwlr_data_control_manager_v1" if state.manager.is_none() => {
                    state.manager = Some(Manager::Wlr(registry.bind::<WlrManager, _, _>(
                        name,
                        version.min(2),
                        qh,
                        (),
                    )))
                }
                "wl_seat" if state.seat.is_none() => {
                    state.seat = Some(registry.bind::<WlSeat, _, _>(name, version.min(7), qh, ()))
                }
                _ => {}
            }
        }
    }
}
wayland_client::delegate_noop!(State: ignore ExtManager);
wayland_client::delegate_noop!(State: ignore WlrManager);
wayland_client::delegate_noop!(State: ignore WlSeat);
macro_rules! data_control {
    ($variant:ident,$device:ty,$offer:ty,$source:ty,$device_events:ident,$offer_events:ident,$source_events:ident) => {
        impl Dispatch<$device,()> for State {
            fn event(state:&mut Self, _: &$device, event:$device_events::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
                match event {
                    $device_events::Event::DataOffer {id} => { state.offers.insert(Offer::$variant(id),Vec::new()); },
                    $device_events::Event::Selection {id} => state.selection(id.map(Offer::$variant)),
                    $device_events::Event::PrimarySelection {id:Some(offer)} => { let offer = Offer::$variant(offer); state.offers.remove(&offer); offer.destroy(); },
                    $device_events::Event::Finished => state.finished = true,
                    _ => {},
                }
            }
            wayland_client::event_created_child!(State,$device,[0 => ($offer,())]);
        }
        impl Dispatch<$offer,()> for State {
            fn event(state:&mut Self, offer:&$offer, event:$offer_events::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
                if let $offer_events::Event::Offer {mime_type} = event {
                    let mimes = state.offers.entry(Offer::$variant(offer.clone())).or_default();
                    if mimes.len() < 64 && mime_type.len() < 128 { mimes.push(mime_type); }
                }
            }
        }
        impl Dispatch<$source,()> for State {
            fn event(state:&mut Self, source:&$source, event:$source_events::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
                let source = Source::$variant(source.clone());
                match event {
                    $source_events::Event::Send {mime_type,fd} if TEXT_MIMES.contains(&mime_type.as_str()) => {
                        if let Some((_,text)) = state.source.as_ref().filter(|(current,_)| current == &source) { state.work.push_back(Work::Write(fd,Arc::clone(text))); }
                    },
                    $source_events::Event::Cancelled if state.source.as_ref().is_some_and(|(current,_)| current == &source) => { state.source.take(); source.destroy(); },
                    _ => {},
                }
            }
        }
    }
}
data_control!(
    Ext,
    ExtDevice,
    ExtOffer,
    ExtSource,
    ext_data_control_device_v1,
    ext_data_control_offer_v1,
    ext_data_control_source_v1
);
data_control!(
    Wlr,
    WlrDevice,
    WlrOffer,
    WlrSource,
    zwlr_data_control_device_v1,
    zwlr_data_control_offer_v1,
    zwlr_data_control_source_v1
);

impl Dispatch<WlCallback, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlCallback,
        _: wayland_client::protocol::wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.registry_ready = true;
    }
}

#[derive(Default)]
pub struct WaylandClipboard {
    connection: Option<Connection>,
}
async fn initialize(
    connection: Option<Connection>,
) -> Result<(Connection, wayland_client::EventQueue<State>, State), String> {
    let connection = match connection {
        Some(connection) => connection,
        None => Connection::connect_to_env()
            .map_err(|_| "Wayland clipboard connection unavailable".to_string())?,
    };
    let mut queue = connection.new_event_queue();
    let mut state = State {
        manager: None,
        seat: None,
        offers: HashMap::new(),
        source: None,
        work: VecDeque::new(),
        finished: false,
        registry_ready: false,
        owned_mime: format!(
            "application/x-ukvm-clipboard-{}-{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ),
    };
    connection.display().get_registry(&queue.handle(), ());
    connection.display().sync(&queue.handle(), ());
    let fd = AsyncFd::new(
        connection
            .backend()
            .poll_fd()
            .try_clone_to_owned()
            .map_err(|_| "Wayland clipboard descriptor unavailable")?,
    )
    .map_err(|_| "Wayland clipboard registration failed")?;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            queue
                .dispatch_pending(&mut state)
                .map_err(|_| "Wayland clipboard registry dispatch failed")?;
            if state.registry_ready {
                return Ok::<_, String>(());
            }
            connection
                .flush()
                .map_err(|_| "Wayland clipboard registry flush failed")?;
            if let Some(guard) = queue.prepare_read() {
                let mut ready = fd
                    .readable()
                    .await
                    .map_err(|_| "Wayland clipboard registry wait failed")?;
                guard
                    .read()
                    .map_err(|_| "Wayland clipboard registry read failed")?;
                ready.clear_ready();
            }
        }
    })
    .await
    .map_err(|_| "Wayland clipboard registry timed out")??;
    Ok((connection, queue, state))
}
pub async fn supported() -> bool {
    initialize(None)
        .await
        .is_ok_and(|(_, _, state)| state.manager.is_some() && state.seat.is_some())
}

async fn read_offer(mut stream: tokio::net::UnixStream) -> ClipboardEvent {
    let mut text = Vec::new();
    match tokio::time::timeout(
        Duration::from_secs(3),
        (&mut stream)
            .take(MAX_CLIPBOARD_BYTES as u64 + 1)
            .read_to_end(&mut text),
    )
    .await
    {
        Ok(Ok(_)) if text.len() <= MAX_CLIPBOARD_BYTES => match String::from_utf8(text) {
            Ok(text) => ClipboardEvent::Text(text),
            Err(_) => ClipboardEvent::Rejected("Clipboard text is not valid UTF-8"),
        },
        Ok(Ok(_)) => ClipboardEvent::Rejected("Clipboard text exceeds 64 KiB"),
        Ok(Err(_)) => ClipboardEvent::Rejected("Clipboard text transfer failed"),
        Err(_) => ClipboardEvent::Rejected("Clipboard text transfer timed out"),
    }
}
async fn write_source(fd: OwnedFd, text: Arc<String>) {
    let Ok(flags) = rustix::fs::fcntl_getfl(&fd) else {
        return;
    };
    if rustix::fs::fcntl_setfl(&fd, flags | rustix::fs::OFlags::NONBLOCK).is_err() {
        return;
    }
    let Ok(fd) = AsyncFd::new(fd) else {
        return;
    };
    let _ = tokio::time::timeout(Duration::from_secs(3), async {
        let mut offset = 0;
        while offset < text.len() {
            let mut ready = fd.writable().await.map_err(|_| ())?;
            match ready.try_io(|inner| {
                rustix::io::write(inner.get_ref(), &text.as_bytes()[offset..])
                    .map_err(std::io::Error::from)
            }) {
                Ok(Ok(0)) | Ok(Err(_)) => return Err(()),
                Ok(Ok(count)) => offset += count,
                Err(_) => {}
            }
        }
        Ok(())
    })
    .await;
}

impl ClipboardBackend for WaylandClipboard {
    async fn run(
        self,
        changes: watch::Sender<Option<ClipboardEvent>>,
        mut writes: watch::Receiver<Option<ClipboardWrite>>,
        mut stop: watch::Receiver<bool>,
    ) -> Result<(), String> {
        let (connection, mut queue, mut state) = initialize(self.connection).await?;
        let manager = state
            .manager
            .clone()
            .ok_or("Wayland data-control clipboard protocol unavailable")?;
        let seat = state
            .seat
            .clone()
            .ok_or("Wayland clipboard seat unavailable")?;
        let device = manager.get_data_device(&seat, &queue.handle(), ());
        let fd = AsyncFd::new(
            connection
                .backend()
                .poll_fd()
                .try_clone_to_owned()
                .map_err(|_| "Wayland clipboard descriptor unavailable")?,
        )
        .map_err(|_| "Wayland clipboard event registration failed")?;
        let mut readers = FuturesUnordered::new();
        let mut writers = FuturesUnordered::new();
        let mut selection = 0u64;
        changes.send_replace(Some(ClipboardEvent::Ready));
        loop {
            if *stop.borrow() {
                break;
            }
            queue
                .dispatch_pending(&mut state)
                .map_err(|_| "Wayland clipboard dispatch failed")?;
            if state.finished {
                return Err("Wayland clipboard session closed".into());
            }
            if state.work.len() > 64 || state.offers.len() > 128 {
                return Err("Wayland clipboard event capacity exceeded".into());
            }
            while let Some(work) = state.work.pop_front() {
                match work {
                    Work::Read(offer, mime) => {
                        selection = selection.wrapping_add(1);
                        if readers.len() >= 16 {
                            offer.destroy();
                            continue;
                        }
                        let (reader, writer) = std::os::unix::net::UnixStream::pair()
                            .map_err(|_| "clipboard transfer pipe failed")?;
                        reader
                            .set_nonblocking(true)
                            .map_err(|_| "clipboard transfer setup failed")?;
                        offer.receive(mime, writer.as_fd());
                        offer.destroy();
                        drop(writer);
                        let stream = tokio::net::UnixStream::from_std(reader)
                            .map_err(|_| "clipboard transfer registration failed")?;
                        let current = selection;
                        readers.push(async move { (current, read_offer(stream).await) });
                    }
                    Work::Write(fd, text) if writers.len() < 16 => {
                        writers.push(write_source(fd, text));
                    }
                    Work::Write(_, _) => {}
                    Work::Clear => {
                        selection = selection.wrapping_add(1);
                        changes.send_replace(Some(ClipboardEvent::Text(String::new())));
                    }
                    Work::Owned => {
                        selection = selection.wrapping_add(1);
                    }
                    Work::Ignored => {
                        selection = selection.wrapping_add(1);
                        changes.send_replace(Some(ClipboardEvent::Ignored));
                    }
                }
            }
            connection
                .flush()
                .map_err(|_| "Wayland clipboard flush failed")?;
            let Some(guard) = queue.prepare_read() else {
                continue;
            };
            tokio::select! {
                _ = stop.changed() => { if *stop.borrow() { break; } },
                result = fd.readable() => {
                    let mut ready = result.map_err(|_| "Wayland clipboard event wait failed")?;
                    match guard.read() { Ok(_) => {}, Err(wayland_client::backend::WaylandError::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock => {}, Err(_) => return Err("Wayland clipboard session lost".into()) }
                    ready.clear_ready();
                },
                result = readers.next(), if !readers.is_empty() => { if let Some((generation,event)) = result { if generation == selection { changes.send_replace(Some(event)); } } },
                _ = writers.next(), if !writers.is_empty() => {},
                result = writes.changed() => {
                    if result.is_err() { break; }
                    if let Some(request) = writes.borrow_and_update().clone() {
                        let Some(_guard) = (request.authorize)() else { continue; };
                        let text = request.text;
                        if text.len() > MAX_CLIPBOARD_BYTES { continue; }
                        selection = selection.wrapping_add(1);
                        let source = manager.create_data_source(&queue.handle(), ());
                        for mime in TEXT_MIMES { source.offer(mime.into()); }
                        source.offer(state.owned_mime.clone());
                        device.set_selection(Some(&source));
                        if let Some((old,_)) = state.source.replace((source, Arc::new(text))) { old.destroy(); }
                        connection.flush().map_err(|_| "Wayland clipboard write failed")?;
                    }
                }
            }
        }
        device.destroy();
        if let Some((source, _)) = state.source.take() {
            source.destroy();
        }
        let _ = connection.flush();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;
    #[tokio::test]
    async fn native_offer_reader_is_utf8_bounded_and_preserves_empty_text() {
        for bytes in [
            Vec::new(),
            "你好 🦀".as_bytes().to_vec(),
            vec![0xff],
            vec![b'x'; MAX_CLIPBOARD_BYTES + 1],
        ] {
            let (mut writer, reader) = tokio::net::UnixStream::pair().unwrap();
            let expected = String::from_utf8(bytes.clone())
                .ok()
                .filter(|text| text.len() <= MAX_CLIPBOARD_BYTES);
            let sending = async {
                let _ = writer.write_all(&bytes).await;
                drop(writer);
            };
            let ((), event) = tokio::join!(sending, read_offer(reader));
            match event {
                ClipboardEvent::Text(text) => assert_eq!(Some(text), expected),
                ClipboardEvent::Rejected(_) => assert!(expected.is_none()),
                _ => panic!("unexpected event"),
            }
        }
    }
    #[tokio::test]
    async fn native_source_serves_unicode_and_empty_without_subprocesses() {
        for text in ["", "world", "árbol 世界"] {
            let (reader, writer) = std::os::unix::net::UnixStream::pair().unwrap();
            reader.set_nonblocking(true).unwrap();
            let mut reader = tokio::net::UnixStream::from_std(reader).unwrap();
            let sending = write_source(writer.into(), Arc::new(text.into()));
            let receiving = async {
                let mut received = String::new();
                reader.read_to_string(&mut received).await.unwrap();
                assert_eq!(received, text);
            };
            tokio::join!(sending, receiving);
        }
    }
}

#[cfg(test)]
#[path = "wayland/tests.rs"]
mod compositor_tests;
