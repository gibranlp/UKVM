# Architecture

## Cross-platform runtime foundation (M22, in progress)

`ukvm-runtime::session` owns the backend-independent exclusive focus gate and
prepared injector leases used by the working Wayland runtime.
`ukvm-runtime::lifecycle` owns cooperative cancellation and retry pauses;
platform signal registration remains explicitly scoped. `ukvm-runtime::peers`
owns ephemeral discovery hints, pinned connection preparation, independent
readiness workers, availability/connection pools and source-owned adjacency
checks. Wayland retains capture geometry, focus transitions and portal/EI
authority, accessing shared peer state through explicit composition. These
internal modules are compiled for the current Linux runtime and for native
portable-policy tests.
The foreground entry point still requires Linux Wayland. Native Windows/macOS
capture, injection, ownership and service adapters are not implemented yet.
See [implementation sequence and progress](CROSS_PLATFORM_PLAN.md).

## Scope and terminology

A **node** is one installed `ukvm` process with both controller and controlled capabilities available subject to its configuration and platform permissions. A **controller** captures local input after a boundary transition; a **controlled** node injects authenticated input into its own session. A node may do both. A **peer** is another node. **Discovery** locates candidate peers and never grants trust. **Pairing** establishes trust with explicit user authorization. **Focus** is the node currently receiving the user's input. A **session** is one authorized control relationship, not a remote desktop stream.

The runtime flow is: validate layout and peer trust -> establish an authenticated encrypted session -> receiver authorizes and prepares injection -> local edge activates capture -> send normalized entry position -> forward ordered input -> controlled node detects an intentional crossing of its inverse outer edge -> release remote held keys/buttons -> send return control -> release local capture near the originating edge. The active controller must not swallow input until the destination accepts the session. On disconnect, injection stops and local capture releases. The controlled receiver has explicit authenticated, authorizing, receiving and releasing states; `ukvm-runtime` now coordinates exclusive local/outgoing/incoming focus, and M14 routes focus across configured adjacent coordinates from the physical source.

## Workspace

| Crate | Responsibility |
| --- | --- |
| `ukvm` (`crates/ukvm-cli`) | CLI and foreground process entry point |
| `ukvm-runtime` | Persistent user-session orchestration, focus ownership, discovery hints, readiness, reconnect and shutdown |
| `ukvm-core` | Platform and transport independent roles, focus and layout concepts |
| `ukvm-config` | User config location, node preferences and node-level layout; no trust or secrets |
| `ukvm-protocol` | Semantic input events and provisional versioned debug-session messages |
| `ukvm-platform` | Input and display traits; eventual OS backends |
| `ukvm-network` | Mutual TLS transport, separate first-contact pairing, bounded framing, persistent identity, pinned peer store, authorization and DebugSink |

`ukvm-discovery` implements DNS-SD advertisement and bounded browsing outside transport and platform input code. Its results are untrusted pairing or control endpoint hints. Service integration remains future work. Pairing now lives beside transport in `ukvm-network` because it creates the same trusted-peer records consumed by mTLS. Avoid empty crates. A future daemon can reuse the core, platform and network crates, while the CLI talks to it through a local authenticated control interface. The CLI should never contain OS input logic. `ukvm setup` orchestrates the existing identity, discovery, pairing, trust, permission and layout APIs; its decision model is separate from terminal prompts. A discovered ID is never trusted without pairing or pinned mTLS verification.

## Platform boundary

`ukvm-platform` exposes `InputSource`, `PlatformInputInjector`, `DisplayService`, and `BoundaryDetector` traits, plus an optional `ClipboardService` interface. `ukvm-core::controlled::InputSink` is the network-facing session boundary. The DebugSink counts events; `RemoteInputSink` adapts a platform injector and checks its per-operation capabilities. Backends must report capabilities and errors at runtime because a platform name alone does not imply permission or compositor support. Display coordinates and pointer deltas are explicit; backend code maps native key codes and display spaces to protocol semantics. The `linux_wayland` module is behind a Linux compilation gate; future `linux_x11`, `macos`, and `windows` modules should follow the same boundary. On Linux, choose Wayland or X11 based on the active session and validate availability; never silently use XWayland to bypass Wayland limits.

The `ukvm-platform::linux_wayland` module probes the live session, creates a permissioned InputCapture portal session, retrieves zones, calculates external edge segments, sets barriers and reads EI events through `reis`. It also implements an experimental `WaylandInputInjector` that requests RemoteDesktop consent, connects an EI sender, and maps normalized events to granted devices. The injector runs on a dedicated thread behind the platform trait; the network layer only sees `RemoteInputSink`. The remote listener keeps its injector actor and RemoteDesktop proxy alive across peer sessions. It parks the permissioned EI session between sessions, releases held input and resets remote pointer state, and closes the portal session when the listener ends; a second physical session reused the grant without another picker. For natural return, the receiver uses the EI device's advertised logical regions, places the cursor through EI absolute motion at the configured entry edge, then owns a virtual pointer state for subsequent absolute motion and inverse-edge detection. The portal exposes no authoritative cursor readback, so this state may diverge under compositor constraints; spectrum → io → spectrum return has been physically proven on the current layout. The generic `InputSource` adapter, cross-platform key translation and non-Linux OS service packaging remain future work. Linux user-service packaging is described in [M15 daily runtime](SERVICE.md). `ukvm doctor` separates portal-advertised capabilities from permission and physical verification. The packaged Hyprland portal does not expose RemoteDesktop, so injector construction fails clearly until a suitable permissioned portal backend is active. See [the opt-in trial](WAYLAND_INJECTION_TRIAL.md).

## Runtime and background operation

`ukvm run` delegates to the reusable `ukvm-runtime` crate. The foreground runtime owns concurrent receiver, controller and discovery futures in one structured `tokio::join!`, plus a single registered SIGINT future. Workers receive cancellation and execute cleanup; active platform and receiver futures are not simply dropped. Transport sender tasks have cancellation ownership, and the injector actor thread is joined when dropped. The diagnostic commands remain available.

A node has explicit Local, Armed, Controlling, Yielding, Reserved, Controlled and Disarming focus states. The outgoing worker first verifies the layout peer's pinned mTLS identity, requests remote input, and waits for authorization, portal consent and EI readiness. Only then can it arm an InputCapture barrier. A received entry can disarm an idle outgoing barrier and wait for its Close before injection; active conflicting entries are rejected. Authorized but idle network relationships do not acquire input focus. Each node has at most one active input recipient relationship.

Receiver portal/EI authority is acquired lazily for a permissioned peer and retained between sessions. Outgoing capture is recreated after return or disconnect. Emergency release first latches a movement guard, ends the remote session and closes capture; another capture is suppressed until the platform observes the local pointer 32 logical pixels inward from the launching edge. The Hyprland observer uses bounded, read-only IPC; failure leaves the guard latched. Discovery addresses remain ephemeral hints; every connection verifies the pin again. Retry delay grows from 1 to 16 seconds and resets after successful session completion. See [runtime lifecycle and limitations](RUNTIME.md).

The current runtime supports Linux Wayland and up to eight cardinal coordinates, two positions in each direction. Independent peer workers prepare pinned/permissioned readiness and a return standby while active; one InputCapture session arms ready adjacent edges. A physical source retains capture across remote-to-remote handoffs and selects the next directly authenticated destination by coordinate adjacency. Up to 16 incoming relationships share one permissioned grant through exclusive entered leases. See [M14 topology](TOPOLOGY.md). Configuration/layout are read at startup; peer permissions are rechecked during operation. M15 packages the same runtime as a Linux systemd user service, activated only by explicit user actions within a real graphical session. It adds a kernel-held per-user lock and an advisory bounded read-only status snapshot, not a control IPC API. Input APIs continue to require the logged-in graphical session; see [service lifecycle](SERVICE.md).

## Configuration

`ukvm-config` uses TOML and OS-appropriate user paths: XDG config directory on Linux, Application Support on macOS, and AppData Roaming on Windows. The schema contains node name, role, listen address and node-level edge links. Links store the peer's certificate fingerprint, not its alias. On Linux, trust metadata and pinned peer certificates live in `$XDG_STATE_HOME/ukvm` (or `~/.local/state/ukvm`), while the unchanged `node.der`/`node.key` identity remains in the existing local user-data directory (`$XDG_DATA_HOME/ukvm` or its platform default). Peer aliases are labels; addresses are replaceable connection hints. Adding a peer defaults to connect allowed, debug input denied and control-this-node denied. The configured `listen` command checks the authenticated certificate ID and the requested session type against those permissions. CLI peer and layout edits take an advisory user-state lock, then replace files through synced temporary files and atomic rename. Coordinate topology is stored in `[[layout.nodes]]`; legacy edge links map to distance-one coordinates. Input Protocol 1.3 carries bounded permitted exits and refreshes without conveying identity or permission. Permissioned portal zone geometry supplies external boundary segments; a monitor-mapping UI remains future work. Existing minimal TOML and identity files remain valid without migration.

Pairing is a temporary first-contact channel, separate from mTLS input sessions. It exchanges public identities inside Noise transport, then requires both users to compare a short transcript-bound value and confirm. Its commit phase writes the same peer store used by manual import; permissions and layout remain separate. A separate, time-limited pending record captures mutual confirmation and commit state so an interrupted distributed commit can be inspected and recovered without turning pending data into trust. See [pairing](PAIRING.md) for the threat model and protocol. See [discovery](DISCOVERY.md) for the temporary mDNS pairing advertisement and bounded browse.

## Interactive Remote Sessions v1

In addition to physical capture, `ukvm remote PEER` provides a native window input
source. Its private local runtime bridge reserves the same focus gate and uses
the same authenticated input Sender and shared RemoteDesktop/EIS injector. Video
uses an independent ScreenCast → restricted PipeWire → JPEG → display TLS → native
window path; display failure does not fail normal runtime workers. Portable frame
and mapping types live in protocol/core; Linux capture/presentation remain in
platform. See [the implementation and limitations](REMOTE_SESSIONS.md). Input 1.4
is required on both nodes; previous milestone protocol references are historical.
