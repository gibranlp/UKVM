# Architecture

## Scope and terminology

A **node** is one installed `ukvm` process with both controller and controlled capabilities available subject to its configuration and platform permissions. A **controller** captures local input after a boundary transition; a **controlled** node injects authenticated input into its own session. A node may do both. A **peer** is another node. **Discovery** locates candidate peers and never grants trust. **Pairing** establishes trust with explicit user authorization. **Focus** is the node currently receiving the user's input. A **session** is one authorized control relationship, not a remote desktop stream.

The eventual runtime flow is: local edge activation -> validate layout and peer trust -> establish an authenticated encrypted session -> send control-enter -> capture local input -> forward ordered input -> inject on the active peer -> control-leave or disconnect -> release held keys/buttons and return local control. The active controller must not swallow input until the destination confirms entry. On disconnect, injection stops and all held remote inputs are released. The controlled receiver now has explicit authenticated, authorizing, receiving and releasing states; full focus handoff remains future work.

## Workspace

| Crate | Responsibility |
| --- | --- |
| `ukvm` (`crates/ukvm-cli`) | CLI and eventual process entry point |
| `ukvm-core` | Platform and transport independent roles, focus and layout concepts |
| `ukvm-config` | User config location, node preferences and node-level layout; no trust or secrets |
| `ukvm-protocol` | Semantic input events and provisional versioned debug-session messages |
| `ukvm-platform` | Input and display traits; eventual OS backends |
| `ukvm-network` | Mutual TLS transport, separate first-contact pairing, bounded framing, persistent identity, pinned peer store, authorization and DebugSink |

`ukvm-discovery` implements short-lived DNS-SD advertisement and browsing outside transport and platform input code. Its results are untrusted pairing endpoint hints. Service integration remains future work. Pairing now lives beside transport in `ukvm-network` because it creates the same trusted-peer records consumed by mTLS. Avoid empty crates. A future daemon can reuse the core, platform and network crates, while the CLI talks to it through a local authenticated control interface. The CLI should never contain OS input logic. `ukvm setup` orchestrates the existing identity, discovery, pairing, trust, permission and layout APIs; its decision model is separate from terminal prompts. A discovered ID is never trusted without pairing or pinned mTLS verification.

## Platform boundary

`ukvm-platform` exposes `InputSource`, `PlatformInputInjector`, `DisplayService`, and `BoundaryDetector` traits, plus an optional `ClipboardService` interface. `ukvm-core::controlled::InputSink` is the network-facing session boundary. The DebugSink counts events; `RemoteInputSink` adapts a platform injector and checks its per-operation capabilities. Backends must report capabilities and errors at runtime because a platform name alone does not imply permission or compositor support. Display coordinates and pointer deltas are explicit; backend code maps native key codes and display spaces to protocol semantics. The `linux_wayland` module is behind a Linux compilation gate; future `linux_x11`, `macos`, and `windows` modules should follow the same boundary. On Linux, choose Wayland or X11 based on the active session and validate availability; never silently use XWayland to bypass Wayland limits.

The `ukvm-platform::linux_wayland` module probes the live session, creates a permissioned InputCapture portal session, retrieves zones, calculates external edge segments, sets barriers and reads EI events through `reis`. It also implements an experimental `WaylandInputInjector` that requests RemoteDesktop consent, connects an EI sender, and maps normalized events to granted devices. The injector runs on a dedicated thread behind the platform trait; the network layer only sees `RemoteInputSink`. The generic `InputSource` adapter, compositor-tested key translation and service runtime remain future work. `ukvm doctor` separates portal-advertised capabilities from permission and physical verification. The packaged Hyprland portal does not expose RemoteDesktop, so injector construction fails clearly until a suitable permissioned portal backend is active. See [the opt-in trial](WAYLAND_INJECTION_TRIAL.md).

## Runtime and background operation

The current CLI offers separate `listen`, `connect`, synthetic debug send and Wayland capture-send development commands. Both directions use the same binary. The listener counts normalized events and cannot inject them. The Wayland capture callback forwards events into a bounded queue only after an authenticated DebugSink accepts the session; it polls transport health and releases capture on loss. See [network transport](NETWORK.md). This is a CLI vertical slice, not the eventual always-running node.

The future long-running node owns both listening and outgoing connections and can become controller, controlled, or both without changing binary. A local IPC endpoint will let `ukvm status`, `stop`, `pair`, and `devices` manage it. OS service packaging (systemd user service, macOS LaunchAgent, Windows user-session background process or suitable service companion) comes later. Input APIs often require the logged-in graphical session, so service design must preserve that context.

## Configuration

`ukvm-config` uses TOML and OS-appropriate user paths: XDG config directory on Linux, Application Support on macOS, and AppData Roaming on Windows. The schema contains node name, role, listen address and node-level edge links. Links store the peer's certificate fingerprint, not its alias. On Linux, trust metadata and pinned peer certificates live in `$XDG_STATE_HOME/ukvm` (or `~/.local/state/ukvm`), while the unchanged `node.der`/`node.key` identity remains in the existing local user-data directory (`$XDG_DATA_HOME/ukvm` or its platform default). Peer aliases are labels; addresses are replaceable connection hints. Adding a peer defaults to connect allowed, debug input denied and control-this-node denied. The configured `listen` command checks the authenticated certificate ID and the requested session type against those permissions. CLI peer and layout edits take an advisory user-state lock, then replace files through synced temporary files and atomic rename. Layout currently maps one peer per edge; monitor-level geometry remains future work. Existing minimal TOML and identity files remain valid without migration.

Pairing is a temporary first-contact channel, separate from mTLS input sessions. It exchanges public identities inside Noise transport, then requires both users to compare a short transcript-bound value and confirm. Its commit phase writes the same peer store used by manual import; permissions and layout remain separate. A separate, time-limited pending record captures mutual confirmation and commit state so an interrupted distributed commit can be inspected and recovered without turning pending data into trust. See [pairing](PAIRING.md) for the threat model and protocol. See [discovery](DISCOVERY.md) for the temporary mDNS pairing advertisement and bounded browse.
