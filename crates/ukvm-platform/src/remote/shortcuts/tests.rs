use super::*;
use std::{
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::{Duration, Instant},
};
use wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::server::{
    zwp_keyboard_shortcuts_inhibit_manager_v1::{
        self as manager, ZwpKeyboardShortcutsInhibitManagerV1 as ServerManager,
    },
    zwp_keyboard_shortcuts_inhibitor_v1::{
        self as inhibitor, ZwpKeyboardShortcutsInhibitorV1 as ServerInhibitor,
    },
};
use wayland_server::{
    protocol::{wl_compositor, wl_seat as seat, wl_surface},
    Client, DataInit, Display, DisplayHandle, GlobalDispatch, New,
};

struct Server {
    acquired: Arc<AtomicUsize>,
    released: Arc<AtomicUsize>,
    grant: bool,
}
macro_rules! global {
    ($proxy:ty) => {
        impl GlobalDispatch<$proxy, ()> for Server {
            fn bind(
                _: &mut Self,
                _: &DisplayHandle,
                _: &Client,
                proxy: New<$proxy>,
                _: &(),
                init: &mut DataInit<'_, Self>,
            ) {
                init.init(proxy, ());
            }
        }
    };
}
global!(ServerManager);
global!(wl_compositor::WlCompositor);
impl GlobalDispatch<seat::WlSeat, ()> for Server {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        proxy: New<seat::WlSeat>,
        _: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(proxy, ())
            .capabilities(seat::Capability::Keyboard);
    }
}
impl wayland_server::Dispatch<ServerManager, ()> for Server {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &ServerManager,
        request: manager::Request,
        _: &(),
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        if let manager::Request::InhibitShortcuts { id, .. } = request {
            state.acquired.fetch_add(1, Ordering::SeqCst);
            let inhibitor = init.init(id, ());
            if state.grant {
                inhibitor.active();
            } else {
                inhibitor.inactive();
            }
        }
    }
}
impl wayland_server::Dispatch<ServerInhibitor, ()> for Server {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &ServerInhibitor,
        request: inhibitor::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if matches!(request, inhibitor::Request::Destroy) {
            state.released.fetch_add(1, Ordering::SeqCst);
        }
    }
}
impl wayland_server::Dispatch<wl_compositor::WlCompositor, ()> for Server {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &wl_compositor::WlCompositor,
        request: wl_compositor::Request,
        _: &(),
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        if let wl_compositor::Request::CreateSurface { id } = request {
            init.init(id, ());
        }
    }
}
macro_rules! server_noop {
    ($proxy:ty, $event:ty) => {
        impl wayland_server::Dispatch<$proxy, ()> for Server {
            fn request(
                _: &mut Self,
                _: &Client,
                _: &$proxy,
                _: $event,
                _: &(),
                _: &DisplayHandle,
                _: &mut DataInit<'_, Self>,
            ) {
            }
        }
    };
}
server_noop!(seat::WlSeat, seat::Request);
server_noop!(wl_surface::WlSurface, wl_surface::Request);
wayland_client::delegate_noop!(State: ignore wayland_client::protocol::wl_compositor::WlCompositor);
wayland_client::delegate_noop!(State: ignore WlSurface);

fn exercise(advertise: bool, grant: bool) {
    let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
    let mut display = Display::<Server>::new().unwrap();
    let mut handle = display.handle();
    handle.insert_client(server, Arc::new(())).unwrap();
    let compositor_global = handle.create_global::<Server, wl_compositor::WlCompositor, _>(1, ());
    handle.create_global::<Server, seat::WlSeat, _>(7, ());
    if advertise {
        handle.create_global::<Server, ServerManager, _>(1, ());
    }
    let acquired = Arc::new(AtomicUsize::new(0));
    let released = Arc::new(AtomicUsize::new(0));
    let mut state = Server {
        acquired: acquired.clone(),
        released: released.clone(),
        grant,
    };
    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    let worker = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !server_stop.load(Ordering::SeqCst) && Instant::now() < deadline {
            display.dispatch_clients(&mut state).unwrap();
            display.flush_clients().unwrap();
            std::thread::sleep(Duration::from_millis(1));
        }
    });
    let owner = Connection::from_socket(client).unwrap();
    let mut owner_queue = owner.new_event_queue::<State>();
    let registry = owner.display().get_registry(&owner_queue.handle(), ());
    // Registry IDs are monotonically assigned; wl_compositor is first.
    let _ = compositor_global;
    let compositor = registry.bind::<wayland_client::protocol::wl_compositor::WlCompositor, _, _>(
        1,
        1,
        &owner_queue.handle(),
        (),
    );
    let surface = compositor.create_surface(&owner_queue.handle(), ());
    owner_queue.roundtrip(&mut State::default()).unwrap();
    // Emulate winit owning the display and surface while our guest owns only
    // the inhibitor. These handles remain live until the guest has dropped.
    let guest = unsafe { Backend::from_foreign_display(owner.backend().display_ptr()) };
    let connection = Connection::from_backend(guest);
    let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), surface.id().as_ptr()) }.unwrap();
    let borrowed = WlSurface::from_id(&connection, id).unwrap();
    let queue = connection.new_event_queue();
    let registry = connection.display().get_registry(&queue.handle(), ());
    let mut shortcuts = Shortcuts {
        state: State::default(),
        queue,
        connection,
        surface: borrowed,
        registry,
        _window: None,
    };
    shortcuts.connection.flush().unwrap();
    owner_queue.roundtrip(&mut State::default()).unwrap();
    shortcuts
        .queue
        .dispatch_pending(&mut shortcuts.state)
        .unwrap();
    shortcuts.connection.flush().unwrap();
    owner_queue.roundtrip(&mut State::default()).unwrap();
    shortcuts
        .queue
        .dispatch_pending(&mut shortcuts.state)
        .unwrap();
    shortcuts.sync(false).unwrap();
    assert_eq!(acquired.load(Ordering::SeqCst), 0);
    shortcuts.sync(true).unwrap();
    shortcuts.connection.flush().unwrap();
    owner_queue.roundtrip(&mut State::default()).unwrap();
    shortcuts
        .queue
        .dispatch_pending(&mut shortcuts.state)
        .unwrap();
    shortcuts.sync(true).unwrap();
    shortcuts.connection.flush().unwrap();
    owner_queue.roundtrip(&mut State::default()).unwrap();
    shortcuts
        .queue
        .dispatch_pending(&mut shortcuts.state)
        .unwrap();
    assert_eq!(acquired.load(Ordering::SeqCst), usize::from(advertise));
    assert_eq!(
        shortcuts.status(),
        if !advertise {
            "local shortcuts unavailable"
        } else if grant {
            "system shortcuts forwarded"
        } else {
            "waiting for compositor shortcut grant"
        }
    );
    shortcuts.release();
    shortcuts.connection.flush().unwrap();
    owner_queue.roundtrip(&mut State::default()).unwrap();
    shortcuts
        .queue
        .dispatch_pending(&mut shortcuts.state)
        .unwrap();
    assert_eq!(released.load(Ordering::SeqCst), usize::from(advertise));
    shortcuts.sync(true).unwrap();
    shortcuts.connection.flush().unwrap();
    owner_queue.roundtrip(&mut State::default()).unwrap();
    shortcuts
        .queue
        .dispatch_pending(&mut shortcuts.state)
        .unwrap();
    drop(shortcuts);
    // Dropping a guest restores shortcuts without destroying the host window
    // or closing its connection. The host can still use/destroy the surface.
    surface.commit();
    owner_queue.roundtrip(&mut State::default()).unwrap();
    assert_eq!(released.load(Ordering::SeqCst), 2 * usize::from(advertise));
    surface.destroy();
    owner_queue.roundtrip(&mut State::default()).unwrap();
    stop.store(true, Ordering::SeqCst);
    worker.join().unwrap();
}

#[test]
fn compositor_grant_release_resume_and_guest_cleanup() {
    exercise(true, true);
}
#[test]
fn compositor_refusal_does_not_claim_shortcuts_forwarded() {
    exercise(true, false);
}
#[test]
fn unsupported_compositor_keeps_viewing_available() {
    exercise(false, false);
}
