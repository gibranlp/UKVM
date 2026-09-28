# Roadmap

This sequence is directional, not a release commitment.

1. **Foundation (complete):** buildable Rust workspace, CLI skeleton, config schema, input traits, protocol concepts, security and platform plans.
2. **Wayland capture foundation (proven on Hyprland):** permissioned zones, outer-edge barriers, activation, EI pointer/keyboard events and Release. Zone-change recovery, generic trait integration and compositor coverage remain.
3. **Wayland injection investigation (complete on current machine):** active portal lacks RemoteDesktop, so no legitimate controlled-node injection was proven. A suitable compositor/backend is needed for that vertical slice.
4. **Development transport (proven across two physical computers):** mutual TLS, manual public-certificate exchange, bounded provisional protocol, heartbeat and DebugSink. A receiver-kill capture release case is proven; reconnect behavior remains untested.
5. **Controlled node foundation (complete):** distinct debug and remote session types, receiver state and sink/injector interfaces.
6. **Persistent peers and node layout (implemented; physical workflow pending retest):** separate config/state/identity storage, pinned peer aliases, default-deny input permissions and configured DebugSink control by edge. Pairing, real receiver injection where supported, focus handoff and held-state recovery remain.
7. **Additional backends and service integration:** Linux X11, macOS, Windows and multiple Wayland compositors; automatic background operation in the user's graphical session.
8. **Discovery and layout expansion:** LAN discovery separate from transport, multi-monitor mapping.
9. **Later features:** text and rich clipboard, file and drag/drop transfer, GUI/TUI, hotkeys, connection statistics. Each requires its own security and compatibility review.
