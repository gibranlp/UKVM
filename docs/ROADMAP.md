# Roadmap

This sequence is directional, not a release commitment.

1. **Foundation (complete):** buildable Rust workspace, CLI skeleton, config schema, input traits, protocol concepts, security and platform plans.
2. **Wayland capture foundation (proven on Hyprland):** permissioned zones, outer-edge barriers, activation, EI pointer/keyboard events and Release. Zone-change recovery, generic trait integration and compositor coverage remain.
3. **Wayland injection investigation (complete on current machine):** active portal lacks RemoteDesktop, so no legitimate controlled-node injection was proven. A suitable compositor/backend is needed for that vertical slice.
4. **Development transport (proven across two physical computers):** mutual TLS, manual public-certificate exchange, bounded provisional protocol, heartbeat and DebugSink. A receiver-kill capture release case is proven; reconnect behavior remains untested.
5. **Controlled node foundation (current):** distinct debug and remote session types, receiver state and sink/injector interfaces. Pairing, local authorization policy, real receiver injection where supported, focus handoff and held-state recovery remain.
6. **Additional backends and service integration:** Linux X11, macOS, Windows and multiple Wayland compositors; automatic background operation in the user's graphical session.
7. **Discovery and layout expansion:** LAN discovery separate from transport, more neighbors, above/below edges, multi-monitor mapping.
8. **Later features:** text and rich clipboard, file and drag/drop transfer, GUI/TUI, hotkeys, connection statistics. Each requires its own security and compatibility review.
