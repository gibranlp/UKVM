//! Compositor-approved, surface-scoped keyboard shortcut inhibition.
use std::{collections::HashMap, sync::Arc};
use wayland_client::{
    backend::{Backend, ObjectId},
    protocol::{wl_registry, wl_seat, wl_surface::WlSurface},
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
};
use wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::client::{
    zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1 as Manager,
    zwp_keyboard_shortcuts_inhibitor_v1::{self, ZwpKeyboardShortcutsInhibitorV1 as Inhibitor},
};
use winit::{
    raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle},
    window::Window,
};

struct Seat {
    proxy: wl_seat::WlSeat,
    keyboard: bool,
    inhibitor: Option<Inhibitor>,
    active: bool,
}
#[derive(Default)]
struct State {
    manager: Option<(u32, Manager)>,
    seats: HashMap<u32, Seat>,
}

pub(super) fn wanted(connected: bool, interactive: bool, controlling: bool, focused: bool) -> bool {
    connected && interactive && controlling && focused
}

pub(super) struct Shortcuts {
    state: State,
    queue: EventQueue<State>,
    connection: Connection,
    surface: WlSurface,
    registry: wl_registry::WlRegistry,
    // Keeps both borrowed FFI handles alive until all our proxies/backend drop.
    _window: Option<Arc<Window>>,
}
impl Shortcuts {
    pub(super) fn new(window: Arc<Window>) -> Result<Self, String> {
        let RawDisplayHandle::Wayland(display) =
            window.display_handle().map_err(|e| e.to_string())?.as_raw()
        else {
            return Err("Shortcut inhibition requires Wayland".into());
        };
        let RawWindowHandle::Wayland(surface) =
            window.window_handle().map_err(|e| e.to_string())?.as_raw()
        else {
            return Err("Shortcut inhibition requires a Wayland surface".into());
        };
        // SAFETY: winit supplies live libwayland handles. The retained Arc keeps
        // the window/surface alive; the event loop outlives this guest backend.
        // Guest mode never closes winit's display. We never take over dispatch
        // or destroy the borrowed surface, and only dispatch our own event queue.
        let backend = unsafe { Backend::from_foreign_display(display.display.as_ptr().cast()) };
        let connection = Connection::from_backend(backend);
        // SAFETY: the handle is a wl_surface from this same display, retained
        // above. from_ptr checks its interface and does not transfer ownership.
        let id =
            unsafe { ObjectId::from_ptr(WlSurface::interface(), surface.surface.as_ptr().cast()) }
                .map_err(|e| e.to_string())?;
        let surface = WlSurface::from_id(&connection, id).map_err(|e| e.to_string())?;
        let queue = connection.new_event_queue();
        let registry = connection.display().get_registry(&queue.handle(), ());
        Ok(Self {
            state: State::default(),
            queue,
            connection,
            surface,
            registry,
            _window: Some(window),
        })
    }

    pub(super) fn sync(&mut self, enabled: bool) -> Result<(), String> {
        // winit reads the shared display. Dispatch pending events without
        // roundtrips, blocking reads, a competing reader, or a background thread.
        self.queue
            .dispatch_pending(&mut self.state)
            .map_err(|e| e.to_string())?;
        for seat in self.state.seats.values_mut() {
            if enabled && seat.keyboard {
                if seat.inhibitor.is_none() {
                    if let Some((_, manager)) = &self.state.manager {
                        seat.inhibitor = Some(manager.inhibit_shortcuts(
                            &self.surface,
                            &seat.proxy,
                            &self.queue.handle(),
                            seat.proxy.id(),
                        ));
                    }
                }
            } else {
                seat.clear();
            }
        }
        self.connection.flush().map_err(|e| e.to_string())
    }

    pub(super) fn status(&self) -> &'static str {
        if self.state.manager.is_none() {
            "local shortcuts unavailable"
        } else if self.state.seats.values().any(|s| s.active) {
            "system shortcuts forwarded"
        } else {
            "waiting for compositor shortcut grant"
        }
    }

    pub(super) fn release(&mut self) {
        for seat in self.state.seats.values_mut() {
            seat.clear();
        }
        let _ = self.connection.flush();
    }
}
impl Seat {
    fn clear(&mut self) {
        if let Some(inhibitor) = self.inhibitor.take() {
            inhibitor.destroy();
        }
        self.active = false;
    }
}
impl Drop for Shortcuts {
    fn drop(&mut self) {
        self.release();
        for seat in self.state.seats.values() {
            seat.proxy.release();
        }
        if let Some((_, manager)) = self.state.manager.take() {
            manager.destroy();
        }
        let _ = self
            .connection
            .backend()
            .destroy_object(&self.registry.id());
        let _ = self.connection.flush();
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
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } if interface == Manager::interface().name && state.manager.is_none() => {
                state.manager = Some((name, registry.bind(name, version.min(1), qh, ())));
            }
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } if interface == "wl_seat" && version >= 5 && state.seats.len() < 32 => {
                state.seats.insert(
                    name,
                    Seat {
                        proxy: registry.bind(name, version.min(7), qh, ()),
                        keyboard: false,
                        inhibitor: None,
                        active: false,
                    },
                );
            }
            wl_registry::Event::GlobalRemove { name } => {
                if let Some(mut seat) = state.seats.remove(&name) {
                    seat.clear();
                    seat.proxy.release();
                }
                if state.manager.as_ref().is_some_and(|(id, _)| *id == name) {
                    for seat in state.seats.values_mut() {
                        seat.clear();
                    }
                    if let Some((_, manager)) = state.manager.take() {
                        manager.destroy();
                    }
                }
            }
            _ => {}
        }
    }
}
impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        proxy: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        {
            if let Some(seat) = state.seats.values_mut().find(|s| s.proxy == *proxy) {
                seat.keyboard = capabilities.contains(wl_seat::Capability::Keyboard);
                if !seat.keyboard {
                    seat.clear();
                }
            }
        }
    }
}
impl Dispatch<Inhibitor, ObjectId> for State {
    fn event(
        state: &mut Self,
        proxy: &Inhibitor,
        event: zwp_keyboard_shortcuts_inhibitor_v1::Event,
        id: &ObjectId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let Some(seat) = state
            .seats
            .values_mut()
            .find(|s| s.proxy.id() == *id && s.inhibitor.as_ref() == Some(proxy))
        {
            seat.active = matches!(event, zwp_keyboard_shortcuts_inhibitor_v1::Event::Active);
        }
    }
}
wayland_client::delegate_noop!(State: ignore Manager);

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_connected_authorized_focused_control_inhibits_shortcuts() {
        assert!(wanted(true, true, true, true));
        for flags in [
            (false, true, true, true),
            (true, false, true, true),
            (true, true, false, true),
            (true, true, true, false),
        ] {
            assert!(!wanted(flags.0, flags.1, flags.2, flags.3));
        }
    }
}

#[cfg(test)]
#[path = "shortcuts/tests.rs"]
mod compositor_tests;
