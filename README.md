# UniversalKVM

UniversalKVM (`ukvm`) is a planned cross-platform software KVM for moving keyboard and mouse control between computers while each uses its own display. One installation can act as controller, controlled machine, or both. Targets are Linux Wayland, Linux X11, macOS, and Windows.

**Current state:** Linux Wayland InputCapture and the development transport are proven across two physical computers. LAN tests carried 528 and 621 pointer events with none dropped and about 1.26 and 1.39 ms heartbeat RTT, respectively. A live receiver termination stopped capture after about 30 pointer events; portal Release succeeded and local input returned. The receiver remains a **DebugSink** and never injects. A separate controlled-session and platform-injector boundary now exists in code, but no real injector or pairing policy is implemented. The active Hyprland portal has no RemoteDesktop interface, so injection is unavailable here. Discovery and the background node are not implemented. `ukvm start`, `stop`, `pair`, and `devices` still return an error.

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

The executable is `target/debug/ukvm`. `ukvm config` prints the default configuration path. An example file is in [`docs/config.example.toml`](docs/config.example.toml); no config is created automatically. Wayland `zones` and `capture` may trigger a portal permission prompt. `capture` reports its portal and EI stages, uses a 60-second safety timeout, and releases five seconds after activation or when Esc or Ctrl-C ends the test. It prints event counts only, never raw keys. If an edge has several outer segments, specify a zone index from `zones`.

For transport testing, exchange each node's **public** `node.der` certificate and verify its fingerprint before use. `ukvm listen --peer-cert PEER.der` listens on loopback by default; add `--bind 0.0.0.0:24800` for a reachable LAN endpoint. `ukvm connect HOST --peer-cert PEER.der` checks authentication and heartbeat RTT. `ukvm debug send-test-events HOST --peer-cert PEER.der` sends harmless synthetic events to the DebugSink. `ukvm platform wayland capture right --send HOST --peer-cert PEER.der` forwards a permitted capture session only after the sink accepts it. See [network transport](docs/NETWORK.md) for the full two-machine procedure and current security limits.

## Design

The CLI remains independent of the future background process. Core policy, input backends, configuration, protocol types and transport are separate Rust crates. Discovery and pairing remain future work. See [architecture](docs/ARCHITECTURE.md), [platform support](docs/PLATFORM_SUPPORT.md), [protocol](docs/PROTOCOL.md), [security](docs/SECURITY.md), and [roadmap](docs/ROADMAP.md).

## License

Licensed under [MIT](LICENSE).
