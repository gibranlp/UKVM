# UniversalKVM

UniversalKVM (`ukvm`) is a planned cross-platform software KVM for moving keyboard and mouse control between computers while each uses its own display. One installation can act as controller, controlled machine, or both. Targets are Linux Wayland, Linux X11, macOS, and Windows.

**Current state:** Linux Wayland InputCapture, authenticated transport, pairing, discovery and real cross-PC input have been physically tested between `spectrum` and `io`. With a reversible consent-capable portal draft on io, the user confirmed pointer movement, clicks, keyboard, modifiers, scrolling, emergency release and controller-side recovery after receiver loss. Input Protocol 1.2 natural return by crossing io's left edge has been physically repeated three times on one listener, with local control restored. A timed abrupt sender stop while Shift and a mouse button were held left both PCs with normal local input and no held state. The packaged Hyprland portal lacks RemoteDesktop; see [Wayland injection trial](docs/WAYLAND_INJECTION_TRIAL.md). No background node is implemented.

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

For the experimental real-input path, the controlled node must have a user-authorized RemoteDesktop v2/ConnectToEIS portal. It must explicitly allow the controller with `ukvm peer allow ALIAS control-this-node` and run `ukvm listen --remote --bind 0.0.0.0:24800 --verbose`; the controller runs `ukvm control right`. The receiver never falls back to DebugSink or privileged injection. The controller and receiver must use the same Input Protocol 1.2 build. The first authorized session asks for portal consent; a second physical session on the same listener reused that grant without a picker. A listener restart asks again because no restore token is stored. See the [trial, physical proof and rollback](docs/WAYLAND_INJECTION_TRIAL.md).

For guided first-run configuration, run `ukvm setup` while the other node is in pairing mode; see [setup](docs/SETUP.md). As an alternative to manual certificate exchange, run `ukvm pair` on one node and `ukvm pair --discover` on the other. `ukvm discover` lists pairing candidates without connecting. Direct `ukvm pair HOST` remains available when multicast discovery is unavailable. Compare the same short verification code on both screens and confirm on both terminals. Pairing uses a temporary listener on TCP 24801 and creates the same pinned peer records with **both input permissions denied**. If the final exchange is interrupted, inspect `ukvm pair status` and use `ukvm pair recover` on the listener with `ukvm pair recover HOST [TRANSACTION_ID]` on the initiator. See [discovery](docs/DISCOVERY.md) and [pairing security and safe physical trial](docs/PAIRING.md). Manual `peer add` remains available.

## Design

The CLI remains independent of the future background process. Core policy, input backends, configuration, protocol types, transport and discovery are separate Rust crates. See [architecture](docs/ARCHITECTURE.md), [platform support](docs/PLATFORM_SUPPORT.md), [protocol](docs/PROTOCOL.md), [security](docs/SECURITY.md), [pairing](docs/PAIRING.md), [discovery](docs/DISCOVERY.md), [setup](docs/SETUP.md), and [roadmap](docs/ROADMAP.md).

## License

Licensed under [MIT](LICENSE).
