use super::*;
use std::{
    io::Write,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
};
use wayland_protocols::ext::data_control::v1::server::{
    ext_data_control_device_v1 as ed, ext_data_control_manager_v1 as em,
    ext_data_control_offer_v1 as eo, ext_data_control_source_v1 as es,
};
use wayland_protocols_wlr::data_control::v1::server::{
    zwlr_data_control_device_v1 as wd, zwlr_data_control_manager_v1 as wm,
    zwlr_data_control_offer_v1 as wo, zwlr_data_control_source_v1 as ws,
};
use wayland_server::protocol::wl_seat::WlSeat as ServerSeat;
use wayland_server::{Client, DataInit, Display, DisplayHandle, GlobalDispatch, New, Resource};

struct Compositor {
    pasted: tokio::sync::mpsc::UnboundedSender<(bool, tokio::net::UnixStream)>,
}
impl GlobalDispatch<ServerSeat, ()> for Compositor {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        seat: New<ServerSeat>,
        _: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(seat, ());
    }
}
impl wayland_server::Dispatch<ServerSeat, ()> for Compositor {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &ServerSeat,
        _: wayland_server::protocol::wl_seat::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}
macro_rules! compositor_protocol {
    ($manager:ty,$device:ty,$offer:ty,$source:ty,$manager_events:ident,$device_events:ident,$offer_events:ident,$source_events:ident) => {
        impl GlobalDispatch<$manager, ()> for Compositor {
            fn bind(
                _: &mut Self,
                _: &DisplayHandle,
                _: &Client,
                manager: New<$manager>,
                _: &(),
                init: &mut DataInit<'_, Self>,
            ) {
                init.init(manager, ());
            }
        }
        impl wayland_server::Dispatch<$manager, ()> for Compositor {
            fn request(
                _: &mut Self,
                client: &Client,
                _: &$manager,
                request: $manager_events::Request,
                _: &(),
                display: &DisplayHandle,
                init: &mut DataInit<'_, Self>,
            ) {
                match request {
                    $manager_events::Request::GetDataDevice { id, .. } => {
                        let device = init.init(id, ());
                        let offer = client
                            .create_resource::<$offer, (), Self>(display, 1, ())
                            .unwrap();
                        device.data_offer(&offer);
                        offer.offer("text/plain;charset=utf-8".into());
                        device.selection(Some(&offer));
                    }
                    $manager_events::Request::CreateDataSource { id } => {
                        init.init(id, Mutex::<Vec<String>>::default());
                    }
                    _ => {}
                }
            }
        }
        impl wayland_server::Dispatch<$device, ()> for Compositor {
            fn request(
                state: &mut Self,
                client: &Client,
                device: &$device,
                request: $device_events::Request,
                _: &(),
                display: &DisplayHandle,
                _: &mut DataInit<'_, Self>,
            ) {
                if let $device_events::Request::SetSelection {
                    source: Some(source),
                } = request
                {
                    let offer = client
                        .create_resource::<$offer, (), Self>(display, 1, ())
                        .unwrap();
                    device.data_offer(&offer);
                    for mime in source
                        .data::<Mutex<Vec<String>>>()
                        .unwrap()
                        .lock()
                        .unwrap()
                        .iter()
                    {
                        offer.offer(mime.clone());
                    }
                    device.selection(Some(&offer));
                    let (reader, writer) = std::os::unix::net::UnixStream::pair().unwrap();
                    reader.set_nonblocking(true).unwrap();
                    source.send("text/plain;charset=utf-8".into(), writer.as_fd());
                    state
                        .pasted
                        .send((
                            <$source as Resource>::interface().name.starts_with("ext_"),
                            tokio::net::UnixStream::from_std(reader).unwrap(),
                        ))
                        .unwrap();
                }
            }
        }
        impl wayland_server::Dispatch<$offer, ()> for Compositor {
            fn request(
                _: &mut Self,
                _: &Client,
                _: &$offer,
                request: $offer_events::Request,
                _: &(),
                _: &DisplayHandle,
                _: &mut DataInit<'_, Self>,
            ) {
                if let $offer_events::Request::Receive { fd, .. } = request {
                    std::fs::File::from(fd).write_all(b"local").unwrap();
                }
            }
        }
        impl wayland_server::Dispatch<$source, Mutex<Vec<String>>> for Compositor {
            fn request(
                _: &mut Self,
                _: &Client,
                _: &$source,
                request: $source_events::Request,
                mimes: &Mutex<Vec<String>>,
                _: &DisplayHandle,
                _: &mut DataInit<'_, Self>,
            ) {
                if let $source_events::Request::Offer { mime_type } = request {
                    mimes.lock().unwrap().push(mime_type);
                }
            }
        }
    };
}
compositor_protocol!(
    em::ExtDataControlManagerV1,
    ed::ExtDataControlDeviceV1,
    eo::ExtDataControlOfferV1,
    es::ExtDataControlSourceV1,
    em,
    ed,
    eo,
    es
);
compositor_protocol!(
    wm::ZwlrDataControlManagerV1,
    wd::ZwlrDataControlDeviceV1,
    wo::ZwlrDataControlOfferV1,
    ws::ZwlrDataControlSourceV1,
    wm,
    wd,
    wo,
    ws
);

async fn exercise_protocol(ext: bool) {
    let (client_socket, server_socket) = std::os::unix::net::UnixStream::pair().unwrap();
    let mut display = Display::<Compositor>::new().unwrap();
    let mut handle = display.handle();
    handle.insert_client(server_socket, Arc::new(())).unwrap();
    handle.create_global::<Compositor, ServerSeat, _>(1, ());
    handle.create_global::<Compositor, wm::ZwlrDataControlManagerV1, _>(2, ());
    if ext {
        handle.create_global::<Compositor, em::ExtDataControlManagerV1, _>(1, ());
    } else {
        handle.create_global::<Compositor, wm::ZwlrDataControlManagerV1, _>(2, ());
    }
    let (pasted, mut paste_requests) = tokio::sync::mpsc::unbounded_channel();
    let mut compositor = Compositor { pasted };
    let (stop_tx, stop) = watch::channel(false);
    let mut server_stop = stop.clone();
    let fd = AsyncFd::new(display.as_fd().try_clone_to_owned().unwrap()).unwrap();
    let server = async {
        loop {
            display.dispatch_clients(&mut compositor).unwrap();
            display.flush_clients().unwrap();
            tokio::select! {
                _ = server_stop.changed() => break,
                ready = fd.readable() => ready.unwrap().clear_ready(),
            }
        }
    };
    let backend = WaylandClipboard {
        connection: Some(Connection::from_socket(client_socket).unwrap()),
    };
    let (changes, mut events) = watch::channel(None);
    let (writes, write_rx) = watch::channel(None);
    let run = backend.run(changes, write_rx, stop);
    let exercise = async {
        loop {
            events.changed().await.unwrap();
            if matches!(&*events.borrow_and_update(),Some(ClipboardEvent::Text(text)) if text == "local")
            {
                break;
            }
        }
        let permitted = Arc::new(AtomicBool::new(true));
        for text in ["world 世界", ""] {
            let allowed = Arc::clone(&permitted);
            writes.send_replace(Some(ClipboardWrite {
                text: text.into(),
                authorize: Arc::new(move || {
                    allowed
                        .load(Ordering::Acquire)
                        .then(|| Box::new(()) as Box<dyn Send>)
                }),
            }));
            let (selected_ext, mut reader) = paste_requests.recv().await.unwrap();
            assert_eq!(selected_ext, ext);
            let mut received = String::new();
            reader.read_to_string(&mut received).await.unwrap();
            assert_eq!(received, text);
            assert!(
                tokio::time::timeout(Duration::from_millis(30), events.changed())
                    .await
                    .is_err(),
                "owned selection must never echo as a local change"
            );
        }
        permitted.store(false, Ordering::Release);
        writes.send_replace(Some(ClipboardWrite {
            text: "denied".into(),
            authorize: Arc::new(|| None),
        }));
        assert!(
            tokio::time::timeout(Duration::from_millis(30), paste_requests.recv())
                .await
                .is_err()
        );
        stop_tx.send_replace(true);
    };
    let ((), result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(server, run, exercise)
    })
    .await
    .unwrap();
    assert!(result.is_ok());
}
#[tokio::test]
async fn ext_data_control_owns_serves_and_suppresses_echo_with_revocation() {
    exercise_protocol(true).await;
}
#[tokio::test]
async fn wlr_fallback_owns_serves_and_suppresses_echo_with_revocation() {
    exercise_protocol(false).await;
}
