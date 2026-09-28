# Roadmap

This sequence is directional, not a release commitment.

1. **Foundation (complete):** buildable Rust workspace, CLI skeleton, config schema, input traits, protocol concepts, security and platform plans.
2. **Wayland capture foundation (proven on Hyprland):** permissioned zones, outer-edge barriers, activation, EI pointer/keyboard events and Release. Zone-change recovery, generic trait integration and compositor coverage remain.
3. **Wayland injection investigation (complete on current machine):** active portal lacks RemoteDesktop, so no legitimate controlled-node injection was proven. A suitable compositor/backend is needed for that vertical slice.
4. **Development transport (proven across two physical computers):** mutual TLS, manual public-certificate exchange, bounded provisional protocol, heartbeat and DebugSink. A receiver-kill capture release case is proven; reconnect behavior remains untested.
5. **Controlled node foundation (complete):** distinct debug and remote session types, receiver state and sink/injector interfaces.
6. **Persistent peers and node layout (physically proven):** separate config/state/identity storage, pinned peer aliases, default-deny input permissions and configured DebugSink control by edge. The configured PC A → PC B workflow worked in a physical test.
7. **First-contact pairing (physically proven):** fresh Noise channel, human comparison, two-sided confirmation and reciprocal pinned peer records without manual certificate exchange. Protocol 2 adds inspectable, idempotent recovery for interrupted commits; local tests pass, while a physical recovery regression remains. Real receiver injection where supported, focus handoff and held-state recovery remain.
8. **Additional backends and service integration:** Linux X11, macOS, Windows and multiple Wayland compositors; automatic background operation in the user's graphical session.
9. **Discovery and layout expansion:** LAN discovery separate from transport, multi-monitor mapping.
10. **Later features:** text and rich clipboard, file and drag/drop transfer, GUI/TUI, hotkeys, connection statistics. Each requires its own security and compatibility review.
