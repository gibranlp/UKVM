# UniversalKVM agent guide

Read this file before changing UKVM. Consult the relevant document under `docs/` for the area being changed. Keep documentation aligned with code.

## Product

UniversalKVM (`ukvm`) is a cross-platform software KVM. Each computer keeps its own monitor; moving across a configured screen edge eventually hands keyboard and pointer control to another computer. Every installation contains both controller and controlled capabilities. Role is selected through configuration and runtime state, not by installing separate server/client binaries. Targets: Linux Wayland, Linux X11, macOS and Windows. Wayland is a first-class target.

Terms: **node** = one installation/process; **controller** = node capturing local input for a peer; **controlled** = node injecting authenticated input locally; **peer** = another node; **discovery** = finding candidates, never granting trust; **pairing** = explicit trust authorization; **focus** = current input recipient; **session** = one authorized control relationship.

Physical KVM remains the foundation. M21 adds explicitly authorized interactive remote sessions with a separate display channel and the existing input system. Do not add remote shell or arbitrary commands. Future clipboard, file transfer, drag/drop, GUI/TUI, hotkeys, advanced display mapping and statistics need separate design and approval through normal project review. Do not portray planned features as implemented.

## Current workspace

- `crates/ukvm-cli`: `ukvm` executable and presentation only.
- `crates/ukvm-core`: platform/transport independent roles and focus concepts.
- `crates/ukvm-config`: TOML preferences and OS config path. Never put private keys or trust records in this config.
- `crates/ukvm-protocol`: versioned semantic events and provisional debug-session wire messages.
- `crates/ukvm-network`: mutual TLS transport, separate first-contact pairing, bounded framing, persistent identity, pinned peer trust records, authorization and DebugSink.
- `crates/ukvm-discovery`: untrusted DNS-SD advertisement and browsing for pairing and runtime control endpoints.
- `crates/ukvm-runtime`: foreground `ukvm run` orchestration, exclusive focus, discovery hints, authenticated readiness, reconnect and structured shutdown. M14 scope: Linux Wayland, eight cardinal coordinates at distance 1–2, independent pinned-peer readiness and source-owned adjacency routing. See `docs/TOPOLOGY.md`. M13 normal switching, 10+ round trips and restart/network/active-disconnect recovery are user-confirmed. Emergency release now suppresses rearming until 32 logical pixels of inward movement, using a read-only Hyprland pointer observer; unavailable observation keeps the edge disarmed. M13 including that fix is physically validated; M14 physical acceptance is pending; see `docs/RUNTIME.md`.
- `crates/ukvm-platform`: platform input/display traits, proven Linux Wayland portal/EI capture, and an experimental RemoteDesktop/EI injector. A reversible consent-capable portal draft on io physically delivered pointer, button, scroll and keyboard input from spectrum. Input Protocol 1.2 adds EI-absolute virtual pointer entry and edge return; three consecutive round trips, emergency release, and held-state cleanup after abrupt sender loss were physically tested. The packaged Hyprland portal lacks RemoteDesktop.
- `docs/ARCHITECTURE.md`, `PROTOCOL.md`, `SECURITY.md`, `PLATFORM_SUPPORT.md`, `ROADMAP.md`: design and limitations.

There is no `ukvm-security` crate yet. Add a crate only when concrete code and a stable responsibility warrant it. Discovery and transport remain separate; mDNS results are untrusted address hints, never identity proof, trust, permission or layout. Guided setup reuses identity, pairing, trust, authorization and layout APIs; an already-trusted discovery hint needs pinned TLS verification before its address can be updated. First-contact pairing is a temporary, explicitly confirmed trust bootstrap; it writes the same pinned peer records as manual import and never grants input permission or layout. Pairing recovery evidence is separate from trust, never grants permission, and must be invalidated by explicit peer removal. Configured peer trust lives in user state, separate from preferences and the node private key. Layout links use cryptographic peer IDs, while aliases and addresses are labels and connection hints. Peer recognition never grants input permission; DebugSink and remote-control permissions are separate and default to deny. The diagnostic default `listen` is a development DebugSink, never an injector. `run` explicitly selects remote input with the same authorization and portal consent requirements. `listen --remote` is a separate, explicitly selected mode and requires `control-this-node` permission plus local RemoteDesktop portal consent before injection. Keep the CLI and reusable foreground runtime independent so the same process can support inbound and outbound control and later user-session service packaging.

Input Protocol 1.3 adds bounded `TopologyEntry`/`TopologyExits` for source-owned multi-node routing; the 1.2 `RemoteEntry` and `ReturnControl` behavior remains for diagnostics. Protocol 1.2 added `RemoteEntry` and `ReturnControl` for natural inverse-edge return. Both nodes need the same input-protocol version; Pairing Protocol 2 is separate and unchanged. The controlled Wayland backend requires an EI absolute pointer and uses its advertised regions to maintain a requested virtual position; this is not authoritative compositor cursor state. The remote listener retains one permissioned portal/EI session between authorized peer sessions, releasing held input each time, and closes it on shutdown. M18 requests official persistent portal consent and privately rotates restore tokens when supported; see `docs/PORTAL_PERSISTENCE.md`. Physical proof covers the spectrum-right/io-left layout on the pinned portal draft, not other edges or compositors.

## Security rules

Read `docs/SECURITY.md` before changing network, pairing, protocol, storage or injection code. Treat discovered peers and all network bytes as untrusted. The development DebugSink requires pinned public-certificate trust and mutual TLS; trust may be established by manual verified import or the separately verified pairing ceremony. Real input injection additionally requires explicit local authorization policy. Peer addresses and displayed names do not prove identity. Never invent cryptography, disable peer verification, log keystrokes/secrets, auto-trust discovered machines, or add remote command execution. Protect private keys using suitable OS/user storage; keep them outside config TOML. Bound message sizes, queue depths and rates; malformed packets must be rejected safely. Stop injection and release held keys/buttons on lost focus, permission loss or disconnect. Require a local override path.

## Platform rules

OS input capture, injection, pointer position, displays, boundaries and clipboard belong behind `ukvm-platform` traits and target-specific modules. No OS-specific calls in core, protocol, config, CLI policy or future transport. Report individual capabilities and permission failures at runtime. Do not assume one OS's input or coordinate model applies elsewhere. Map physical keys carefully; test modifier state, button releases, scaling and injected-event loops.

For Wayland, investigate XDG Desktop Portal InputCapture, RemoteDesktop and libei first. Respect compositor and user grants. If a compositor lacks an API, report the limitation. Never force XWayland, disable Wayland security, use privileged `/dev/input` access as a universal workaround, or imply portal support is universal. Do not silently fall back from a native Wayland session to X11. macOS and Windows likewise need sanctioned APIs and session/permission handling.

## Coding and dependencies

Use stable Rust, idiomatic ownership and explicit errors. Keep abstractions small and tied to real seams. Prefer safe Rust; isolate and explain any necessary FFI or `unsafe` in platform modules with invariants and tests. Do not add another implementation language without a clear platform-specific reason. Choose maintained, established crates for transport, cryptography and parsing; review security and licenses. Avoid custom cryptography, bespoke transport framing without a protocol review, unnecessary micro-crates and placeholder implementations that report false success. The current workspace supports Rust 1.87+ and edition 2021; update MSRV deliberately. Linux Wayland portal code uses `ashpd` and `zbus`; EI input uses `reis`. Consult `docs/PLATFORM_SUPPORT.md` for dependency rationale and test evidence.

## Testing and documentation

Run these from the repository root after relevant changes:

```sh
cargo fmt --all -- --check
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace
```

Tests should cover meaningful policy, version compatibility, parsing, malformed input and backend capability behavior. Platform backend claims require tests on actual target OS/session and, for Wayland, representative compositors. Record untested platforms honestly. Keep `README.md` and `docs/` aligned with reality; update protocol versioning and security notes when behavior changes. Do not mark a CLI command or backend operational until it really works end to end.

## Two-computer testing and Git attribution

The user has authorized routine redeployment on the Spectrum development PC
(hostname Hell) after validated UKVM changes. On that PC, install the release
build into ~/.local, regenerate its managed user-service unit using the installed
binary, restart the service, and verify the selected executable and service state.
Do this as part of completing the change, rather than leaving local deployment
commands for the user. This authorization is scoped to that PC; portal consent
and peer permissions remain explicit user choices.

When a change needs to be checked on the other computer, complete local review and validation, then commit and push the relevant code to the configured Git remote before giving instructions to pull or test it there. Verify that the push succeeded and report the commit ID. If it cannot be pushed, say so clearly; do not present an unpublished change as ready for the other computer. Check the staged files for secrets and build output before committing.

Use the repository owner's configured Git identity. Do not add Codex as an author, co-author, contributor, or commit trailer, and do not add Codex to a contributors list.

M14 topology uses `[[layout.nodes]]` x/y/pinned peer_id, with existing edge links interpreted as adjacent positions. Every routed destination directly authenticates/authorizes the physical source; no receiver delegates its permission. Ready connections share one serialized legitimate portal/EI grant, but only an exclusive entered lease may begin/inject/park. Read `docs/TOPOLOGY.md` before changing routing or availability.

Local capture geometry uses permissioned InputCapture portal zones, with all reachable external segments per direction and internal seams removed. Entry/return fractions share a direction-wide logical span; release and emergency hysteresis use the actual launch segment. Geometry calculation is physically confirmed; spectrum left capture remains blocked by the installed Hyprland partial-barrier validator. Lifecycle diagnostics and the exact backend evidence are documented; do not claim both left segments operational or bypass validation. See `docs/TOPOLOGY.md`.


M15 provides Linux `ukvm install`, `service install/enable/start/stop/restart/status/disable/uninstall`, `status` and `peers`. Read `docs/SERVICE.md` for daily runtime/packaging changes. Unit installation/reload does not enable/start; do not operate a real user's service without explicit authorization. Service uses the same permissioned runtime, graphical session target and scoped environment import; no root/system input daemon. Runtime ownership is a permanent-inode kernel flock in a private XDG runtime directory; bounded read-only status snapshots are advisory and never grant authority. SIGINT/SIGTERM shut down cooperatively. M15 normal operation, switching, single-instance protection, restart recovery and logout cleanup physically pass. The login-autostart fix retries readiness and validates the current logind/compositor session; UKVM automatic startup after logout/login physically passes. The persistent pinned development portal lifecycle is physically validated through M15, as reported by the user (see `docs/WAYLAND_INJECTION_TRIAL.md`); the M14 Hyprland partial-barrier blocker remains unchanged. M16 adds Linux management UX and the versioned frontend contract in `docs/FRONTEND_CONTRACT.md`; changes must preserve M15 ownership/startup, trust/permission/consent boundaries and keep the trial separate. Structured pairing requires code-bound approval on both computers. No SpectrumOS widget, macOS/Windows implementation or M17 is authorized.

M18 production-package installation/pairing/RemoteDesktop consent/control and post-reboot recovery were physically validated by the user on a new SpectrumOS laptop. Persistent desktop authorization now uses official RemoteDesktop v2 and InputCapture v2 restore tokens, separate from peer trust, with private state and explicit confirmed reset; read docs/PORTAL_PERSISTENCE.md before touching it. The pinned XDPH backend supports RemoteDesktop persistence with an unchecked explicit remember choice; InputCapture is v1 and cannot persist. Unattended restoration is not physically validated yet. M17's SpectrumOS widget is implemented; old scope notes above are historical. Windows/macOS remain unstarted.

M19 implements separate opt-in plain-text clipboard synchronization; read docs/CLIPBOARD.md and FRONTEND_CONTRACT.md before changing it. Clipboards are sensitive: default deny, pinned TLS, independent peer permissions, bounded UTF-8, no contents in logs/JSON, no relay to third peers. OS clipboard ownership is behind ukvm-platform::clipboard. M18 is now user-confirmed physically accepted; preserve its input/session/portal persistence behavior. M19 Spectrum/Hellgen physical clipboard acceptance remains pending. Windows/macOS remain unstarted.

M20 implements independent opt-in secure file transfer for Linux. Read docs/FILE_TRANSFER.md, FRONTEND_CONTRACT.md and SECURITY.md before changing it. File Protocol 1 uses a separate pinned mutual-TLS ALPN channel, bounded streaming, per-peer receive-files default deny, live policy guards, private staging and SHA-256 verified atomic no-replace publication. Filesystem operations belong behind ukvm-platform::files. Contents/digests never appear in logs or frontend JSON. M18 persistence/control and M19 Spectrum/Hellgen clipboard/widget are user-confirmed physically accepted. M20 physical acceptance is pending; Windows/macOS remain unstarted. Earlier scope/acceptance notes above are historical.

M21 implements Linux Wayland Interactive Remote Sessions v1 (`ukvm remote PEER`): separate default-deny view-screen permission, ScreenCast/PipeWire/JPEG Display Protocol 1, native window input through the existing runtime focus gate, pinned input transport and shared RemoteDesktop/EIS injector. Input Protocol 1.4 is required on both nodes; Pairing 2, Clipboard 1 and File 1 are unchanged. Read docs/REMOTE_SESSIONS.md before changing display/window input. Physical acceptance is pending. No audio, recording, multi-monitor viewing, automatic headless output, Windows or macOS is implemented. Preserve the earlier accepted input/portal/clipboard lifecycle and M20 scope.

M21 follow-up: the user physically confirmed io viewing/window control and that
Spectrum no longer freezes after 26ee11e. Full cleanup/release/regression acceptance
remains pending. ScreenCast v4+ now requests official persistent monitor consent
using its own private single-use credential; unsupported/non-persisting backends
still require local approval. JPEG uses quality 92/full 4:4:4 color and improved
scaling, retaining 1080p/frame/bandwidth bounds. Remembered ScreenCast restoration
and improved image quality require physical acceptance. Confirmed authorization
reset now forgets all three portal credentials; do not reset accepted input
credentials just to test screen sharing.
