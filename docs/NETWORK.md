# Development network transport

The same encrypted two-node transport serves the non-injecting DebugSink and the separately authorized experimental RemoteInputSink. Pairing can populate the trust store without manual certificate transfer. Temporary DNS-SD discovery exists for pairing; no reconnect loop or service runtime exists. The same `ukvm` binary can listen or connect. The listener currently serves one connection at a time.

## Transport choice

UKVM currently uses persistent TCP with mutual TLS 1.3 through `tokio-rustls`/`rustls`. TCP gives ordered, reliable delivery for the first control channel; `TCP_NODELAY` avoids delayed small writes. This works across LAN and private routed networks without subnet assumptions. QUIC through `quinn` is a valid later candidate for multiple independent streams and datagrams, but adds connection and stream policy before UKVM needs them. This choice does not freeze the eventual production transport. There is no plaintext mode or custom cryptography. `rcgen` generates per-node development certificates; SHA-256 of the public DER certificate is the node ID. `bincode` gives compact provisional framing; this format is **not frozen** for cross-version interoperability.

`Cargo.lock` pins the transitive `time` dependency to a release compatible with the workspace's Rust 1.87 MSRV. `dirs` supplies an OS user-state location. Tokio drives asynchronous sockets and bounded channels. These dependencies have concrete responsibilities; the transport crate does not depend on Wayland or any platform input API.

## Identity and manual trust

`ukvm identity` creates `node.der` (public) and `node.key` (private) under the local user data directory's `ukvm` subdirectory. `--state-dir PATH` overrides it for tests. On Unix, the identity directory is mode 0700 and the key is mode 0600; loading rejects a key readable by other users or a non-regular key file. Manual exchange remains possible: exchange **only** `node.der` using a separately authenticated channel, then use `ukvm peer add` and compare its full SHA-256 fingerprint with `ukvm identity` on the remote node. Pairing instead exchanges public certificates inside its short-lived encrypted channel and requires both users to compare the same short value. Both paths store pinned certificates under `$XDG_STATE_HOME/ukvm/peers/` and metadata in `$XDG_STATE_HOME/ukvm/peers.toml` (default `~/.local/state/ukvm` on Linux). A peer alias or IP address never grants trust. `connect` is allowed after import or pairing; `debug-input` and `control-this-node` default to deny and are changed only by an explicit `peer allow` command. Certificate rotation/revocation UI and OS keychain integration do not exist. Do not share `node.key`. See [pairing](PAIRING.md).

The TLS handshake validates the pinned peer certificate, including possession of its private key. The protocol `Hello.node_id` must equal the SHA-256 fingerprint of the authenticated TLS certificate. Version and capability claims are then validated. The configured DebugSink listener checks `connect` permission after authentication and `debug-input` when `SessionStart` arrives. The separate `listen --remote` mode creates a `RemoteInputSink`, advertises only remote input, and requires the receiver's `control-this-node` permission for `RemoteSessionStart`. `control EDGE` requests that session and does not begin capture until `RemoteSessionAccept`, which follows portal consent and EI device readiness.

## Wire and session lifecycle

Each TLS application frame is a four-byte unsigned big-endian payload length followed by a `bincode` fixed-integer message. The maximum payload is 512 bytes; the limit is checked before reading or allocating the body. Decoder limits and validation cover message variants, trailing data, IDs, names, platform strings, finite bounded deltas, edge fractions and key/button ranges. The current provisional input version is `1.2`; any different major or minor version is rejected. The new return-edge messages change the wire enum, so both machines need matching builds. Pairing Protocol 2 is unchanged.

The DebugSink sequence is `Hello`, `SessionStart`/`SessionAccept`, `Input` events and heartbeat/ack, then `SessionEnd`. The remote-input sequence is `Hello`, `RemoteSessionStart`/`RemoteSessionAccept`, `RemoteEntry`, `Input`, and either receiver-originated `ReturnControl` or `SessionEnd`. The receiver releases held input before sending `ReturnControl`, then closes the session. Session state rejects input before entry and unexpected message types. A debug listener reports counts and event rate without printing key values. A synthetic test sends two relative motions and a Shift press/release to this sink only. The receiver limits events to 20,000 per second. No input message can execute a command.

The sender holds at most 256 queued input events. Wayland's event callback uses non-blocking `try_send`: it may drop pointer motion when full, counting drops; failure to queue a key, button or scroll event aborts capture so stateful input is not silently lost. A send/read stall or missing heartbeat for four seconds marks the connection unhealthy. The Wayland capture loop checks health about every 100 ms, calls portal `Release` if active, and then disables/closes the session. That is the safety path for a peer crash or network loss. A DebugSink peer-loss live test succeeded; the remote-injector case remains untested. RTT comes from heartbeat/ack on one connection; it is not a one-way latency measurement. Reported input frame bytes exclude TLS/TCP overhead, Hello and heartbeats. Reconnection requires starting a new command and reauthenticating.

The experimental remote-injection session has also been physically tested on two computers using io's reversible consent-capable Hyprland portal draft. Spectrum sent 1087 pointer, 8 button, and 116 keyboard events through the authenticated connection, with zero pointer drops and about 1.39 ms heartbeat RTT; the user observed io's pointer, clicks and keyboard responding. Spectrum's local portal Release and return of local control were observed. A later short session confirmed the emergency release chord. No scroll events were sent, and remote-injector disconnect cleanup remains untested. See [Wayland injection trial](WAYLAND_INJECTION_TRIAL.md).

`KeyCode::LinuxEvdev` explicitly labels the source code domain. The experimental Linux-to-Linux EI sender forwards that evdev physical code into the receiver's compositor keymap. Compatible layouts are assumed for the first physical proof; cross-platform translation is not implemented. The protocol currently has no session ID, cross-platform key translation, motion coalescing, replay across reconnect or multiple logical streams.

## Commands

For a fresh **Arch Linux** receiver, copy the repository or just [`setup-arch-test-node.sh`](../scripts/setup-arch-test-node.sh) to that machine first. Run the script as a normal user; it uses `sudo pacman -Syu --needed` for a full system update and build packages, then installs stable Rust with rustfmt/Clippy. It builds and runs headless tests if the repository is present. `--dry-run` prints the commands without installing anything. The DebugSink does not require a Wayland compositor or portal backend; this setup does not install or configure one. If you copied only the script, transfer the matching UKVM source afterward and run `cargo build --locked --workspace` in that checkout. Never transfer `node.key` with the source.

For two machines, build the same revision on both. On **each** machine, run `ukvm identity` and exchange the printed public `node.der` files after checking fingerprints. Put the other machine's public certificate at a local path such as `./peer.der`.

On receiver B:

```sh
ukvm listen --bind 0.0.0.0:24800 --peer-cert ./machine-a.der
```

On controller A, check authentication first:

```sh
ukvm connect MACHINE_B_IP:24800 --peer-cert ./machine-b.der
```

Send harmless synthetic events if desired:

```sh
ukvm debug send-test-events MACHINE_B_IP:24800 --peer-cert ./machine-b.der
```

For a Wayland controller capture on A:

```sh
ukvm platform wayland capture right --send MACHINE_B_IP:24800 --peer-cert ./machine-b.der --verbose
```

Move the pointer through the **outer right edge of the large monitor** after the command reports that the remote DebugSink accepted the session and the portal accepted the barrier. UKVM releases after five seconds of capture; Esc/Ctrl-C also stops it. Specify a zone when edge selection is ambiguous. The legacy `--peer-cert` listener defaults to `127.0.0.1:24800`; add `--bind 0.0.0.0:24800` for LAN listening. The configured `ukvm listen` command uses `[network].listen`, defaulting to `0.0.0.0:24800`. Restrict that port to intended peers with ordinary host firewall policy. There is no Internet/cloud requirement.

## Validation evidence

The configured Milestone 8 physical workflow was subsequently tested between PC A (`192.168.1.21`) and PC B (`192.168.1.40`); the user reported right-edge capture through alias, pinned mTLS and B's DebugSink worked flawlessly. This is physical proof of the configured DebugSink path, not input injection. First-contact pairing was also physically proven between these nodes: matching short codes, two human confirmations, reciprocal certificate pins, default-deny input permissions, and no `.der` transfer. Pairing protocol 2 retains nonsecret recovery receipts for interrupted commits; a physical idempotent recovery completed while preserving permissions and layout. DNS-SD now provides untrusted pairing address hints and was physically proven between the two PCs. See [discovery](DISCOVERY.md). See [pairing](PAIRING.md).

### Configured workflow (Milestone 8)

Build the same revision on both machines and run `ukvm identity`. Compare each full SHA-256 fingerprint with the certificate shown during import. On controller A:

```sh
ukvm peer add ./machine-b.der --name io --address 192.168.1.40
ukvm layout set right io
ukvm config check
ukvm status
```

On receiver B:

```sh
ukvm peer add ./machine-a.der --name spectrum --address 192.168.1.21
ukvm peer allow spectrum debug-input
ukvm config check
ukvm listen
```

Then on A run `ukvm control right --debug`. The edge link resolves B's cryptographic ID, address hint and pinned certificate, and uses the existing capture and DebugSink sender. `ukvm connect io` checks heartbeat without repeating the address or certificate path. The configured listener checks local permissions against the authenticated ID. `control-this-node` remains denied and no input is injected. An isolated two-identity localhost CLI smoke test completed peer import, layout configuration, explicit debug permission, config validation, authenticated `connect`, and `debug send-test-events`. The configured physical capture test was later completed successfully across PC A and PC B. Peer permissions are loaded when `ukvm listen` starts, so restart it after changing permissions. The listener handles one connection at a time.

On one Arch Linux/Hyprland machine, two identities authenticated over localhost mutual TLS. `connect` completed Hello and a heartbeat RTT check. The synthetic test sent and received exactly two pointer and two keyboard events. A physical right-edge capture then activated InputCapture, delivered 611 normalized pointer events to the sender, and the DebugSink counted exactly 611 pointer events and zero keyboard events. The portal accepted barrier 1 on zone 0 from `(0, 0)` to `(0, 2159)`. Release succeeded and the user confirmed local mouse/keyboard control returned normally. Two physical computers later carried 528 pointer events with none dropped and about 1.26 ms RTT, then 621 with none dropped and about 1.39 ms RTT. In a separate live disconnect test, terminating the receiver during capture stopped it after about 30 pointer events; portal Release succeeded, the session closed, and local input returned. These results prove the two-computer capture-to-TLS-to-DebugSink path and this disconnect recovery case. No receiver injection occurred.
