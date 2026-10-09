# UniversalKVM

UniversalKVM (`ukvm`) is a planned cross-platform software KVM for moving keyboard and mouse control between computers while each uses its own display. One installation can act as controller, controlled machine, or both. Targets are Linux Wayland, Linux X11, macOS, and Windows.

**Current state:** Linux Wayland InputCapture, authenticated transport, pairing, discovery and real cross-PC input have been physically tested between `spectrum` and `io`. With a reversible consent-capable portal draft on io, the user confirmed pointer movement, clicks, keyboard, modifiers, scrolling, emergency release and controller-side recovery after receiver loss. Input Protocol 1.2 natural return by crossing io's left edge has been physically repeated three times on one listener, with local control restored. A timed abrupt sender stop while Shift and a mouse button were held left both PCs with normal local input and no held state. The packaged Hyprland portal lacks RemoteDesktop; see [Wayland injection trial](docs/WAYLAND_INJECTION_TRIAL.md). `ukvm run` now provides a foreground persistent runtime; M13 normal switching, 10+ round trips and receiver/network/active-disconnect recovery are user-confirmed; M13, including its emergency rearm fix, is complete and physically validated. M14 coordinate topology is implemented with automated tests; multi-node physical acceptance is pending.

## Clipboard sharing

M19 adds opt-in, bidirectional UTF-8 text sharing on Linux Wayland, independent of
pointer focus and remote-control permission. On each trusted node run
`ukvm peer allow OTHER_PEER share-clipboard --json`, or use the separate SpectrumOS
widget control. New peers default OFF. Changes apply live; no contents appear in
diagnostics. Read [clipboard design and acceptance](docs/CLIPBOARD.md).
Spectrum ↔ Hellgen clipboard physical acceptance is pending; M18 Linux input and
portal persistence are physically accepted.

## Continuous use

With the existing identity, trusted peers, explicit `control-this-node` permission on io, and spectrum's right-edge layout, run this on **both** computers:

```sh
ukvm run --verbose
```

The runtime listens, advertises/discovers endpoint hints, verifies pinned mTLS identities, obtains normal portal consent, and rearms after return or failure. It enables the local edge only after the remote injector accepts the session. Offline or rejected peers leave local input usable; reconnection retries automatically. Ctrl+Alt+Shift+Escape releases control; Ctrl+C shuts down. Normal `ukvm run` prints concise startup/shutdown output; verbose mode adds lifecycle events without individual input events. Existing diagnostic commands remain available.

The first authorized incoming session requests RemoteDesktop consent. A valid portal/EI grant is reused until invalidated or shutdown; M18 restores remembered authorization through the official portal persistence API where supported. The runtime supports Linux Wayland with up to eight cardinal positions at distance 1–2. See [topology configuration and physical test plan](docs/TOPOLOGY.md). Emergency release keeps the launching edge disarmed until the local pointer moves at least 32 logical pixels inward. Idle pointer observation currently requires Hyprland; unavailable observation keeps the edge disarmed. M13 is physically validated; M14 multi-node acceptance remains pending. See [runtime design and acceptance procedure](docs/RUNTIME.md).

## Installation and background use

```sh
cargo build --release --workspace
./target/release/ukvm install
export PATH="$HOME/.local/bin:$PATH"
ukvm service install
# Explicitly opt in, from a Wayland graphical terminal:
ukvm service enable
ukvm service start
ukvm status
ukvm peers
journalctl --user -u ukvm
```

Install does not enable/start. The service uses the existing runtime and normal portal consent, with per-user single-instance ownership and cooperative shutdown. Use `ukvm service stop`, `restart`, `disable`, `status` or `uninstall` as needed. Login startup retries session/backend readiness and validates current compositor sockets, including native Hyprland environment recovery. Desktop systemd graphical-session integration is required; see [installed paths, lifecycle and acceptance](docs/SERVICE.md). The Hyprland partial-edge validator blocker remains unchanged; neither service installation nor M15 resolves it.

## Build and inspect

On a fresh Arch Linux test computer, run [`scripts/setup-arch-test-node.sh`](scripts/setup-arch-test-node.sh) as your normal user after copying the script or repository there. It installs the build packages and stable Rust toolchain, then builds and tests UKVM when the checkout is present. Preview its commands with `bash scripts/setup-arch-test-node.sh --dry-run`. The receiving DebugSink does not need Wayland portal packages. See [network transport](docs/NETWORK.md) for the two-node procedure.

Install stable Rust, then run:

```sh
cargo build --workspace
cargo test --workspace
cargo run -p ukvm -- help
cargo run -p ukvm -- doctor
cargo run -p ukvm -- platform wayland zones
cargo run -p ukvm -- platform wayland capture right --verbose
cargo run -p ukvm -- identity
```

The executable is `target/debug/ukvm`. `ukvm config path`, `ukvm config show`, and `ukvm config check` inspect preferences and validate peer references without exposing the private key. An example file is in [`docs/config.example.toml`](docs/config.example.toml); `layout set` creates it when needed. Wayland `zones` and `capture` may trigger a portal permission prompt. `capture` reports its portal and EI stages, uses a 60-second safety timeout, and releases five seconds after activation or when Esc or Ctrl-C ends the test. It prints event counts only, never raw keys. If an edge has several outer segments, specify a zone index from `zones`.

For transport testing, exchange each node's **public** `node.der` certificate and verify its fingerprint before use. `ukvm listen --peer-cert PEER.der` listens on loopback by default; add `--bind 0.0.0.0:24800` for a reachable LAN endpoint. `ukvm connect HOST --peer-cert PEER.der` checks authentication and heartbeat RTT. `ukvm debug send-test-events HOST --peer-cert PEER.der` sends harmless synthetic events to the DebugSink. `ukvm platform wayland capture right --send HOST --peer-cert PEER.der` forwards a permitted capture session only after the sink accepts it. See [network transport](docs/NETWORK.md) for the full two-machine procedure and current security limits.

For the configured workflow, import and verify each other's public certificates once with `ukvm peer add CERT --name ALIAS --address HOST`. On the receiver, explicitly run `ukvm peer allow ALIAS debug-input`, then `ukvm listen`. On the controller, run `ukvm layout set right ALIAS` and `ukvm control right --debug`. This remains a non-injecting DebugSink session. `ukvm peer show ALIAS` displays the full SHA-256 fingerprint; names and addresses are labels and connection hints, never identity.

For the experimental real-input path, the controlled node must have a user-authorized RemoteDesktop v2/ConnectToEIS portal. It must explicitly allow the controller with `ukvm peer allow ALIAS control-this-node` and run `ukvm listen --remote --bind 0.0.0.0:24800 --verbose`; the controller runs `ukvm control right`. The receiver never falls back to DebugSink or privileged injection. The controller and receiver must use the same Input Protocol 1.3 build. The first authorized session asks for portal consent; a second physical session on the same listener reused that grant without a picker. A listener restart asks again because no restore token is stored. See the [trial, physical proof and rollback](docs/WAYLAND_INJECTION_TRIAL.md).

For guided first-run configuration, run `ukvm setup` to prepare identity, platform readiness and the user service, then optionally pair another node; see [setup](docs/SETUP.md). As an alternative to manual certificate exchange, run `ukvm pair` on one node and `ukvm pair --discover` on the other. `ukvm discover` lists pairing and running machines without authenticating them. Direct `ukvm pair HOST` remains available when multicast discovery is unavailable. Compare the same short verification code on both screens and confirm on both terminals. Pairing uses a temporary listener on TCP 24801 and creates the same pinned peer records with **both input permissions denied**. If the final exchange is interrupted, inspect `ukvm pair status` and use `ukvm pair recover` on the listener with `ukvm pair recover HOST [TRANSACTION_ID]` on the initiator. See [discovery](docs/DISCOVERY.md) and [pairing security and safe physical trial](docs/PAIRING.md). Manual `peer add` remains available.

## Linux configuration and frontend integration

M16 adds safe configuration commands without editing TOML:

```sh
ukvm peers
ukvm layout show
ukvm layout place 1 0 io
ukvm layout move io -1 0
ukvm apply
ukvm status --json
ukvm peers --json
ukvm discover --json
ukvm layout --json
ukvm doctor --json
```

Use place for an empty coordinate and move for an existing peer. Coordinates remain cardinal distance 1–2, with an inner neighbor required for outer placements. `ukvm layout remove PEER` and `ukvm unpair PEER` require confirmation or explicit `--yes`; unpair removes local trust and topology, never remote trust. Permission inspection and changes remain explicit `ukvm peer permissions/allow/deny` commands. `apply` validates config and restarts the managed user service, preserving exclusive runtime ownership and normal portal consent.

The [versioned frontend contract](docs/FRONTEND_CONTRACT.md) documents JSON health, machine/topology views, permissions, service actions, useful errors and interactive JSON Lines pairing. Readiness is separate from trust, input permission and portal consent. No SpectrumOS dependency/widget, macOS/Windows work or M17 is included. M16 physical acceptance is pending; M15's proven control/runtime lifecycle is retained.

## Design

The CLI calls the reusable `ukvm-runtime` library for foreground operation; Linux installation, a systemd user service and daily status/peers commands are available in M15; normal service operation, switching, single-instance protection and restart recovery are physically confirmed. Automatic service startup after logout/login is physically confirmed. The persistent pinned development portal lifecycle is physically validated through M15; see [trial lifecycle](docs/WAYLAND_INJECTION_TRIAL.md#m15-logoutlogin-lifecycle-regression). See [service setup](docs/SERVICE.md). Core policy, input backends, configuration, protocol types, transport and discovery are separate Rust crates. See [architecture](docs/ARCHITECTURE.md), [platform support](docs/PLATFORM_SUPPORT.md), [protocol](docs/PROTOCOL.md), [security](docs/SECURITY.md), [pairing](docs/PAIRING.md), [discovery](docs/DISCOVERY.md), [setup](docs/SETUP.md), and [roadmap](docs/ROADMAP.md).

## License

Licensed under [MIT](LICENSE).

Persistent desktop authorization on supported Linux portals is documented in [PORTAL_PERSISTENCE.md](docs/PORTAL_PERSISTENCE.md). Inspect safe state with `ukvm authorization status`; explicitly forget remembered approval with `ukvm authorization reset`. Peer trust and control permissions remain separate. Unattended physical acceptance of this persistence change is pending.
