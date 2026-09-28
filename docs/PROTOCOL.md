# Protocol sketch (version 1.0)

This describes intended semantics and a **provisional development** wire format. `ukvm-protocol` contains the semantic input model, version constants and `WireMessage`; `ukvm-network` implements bounded framing. Use matching builds for manual tests. See [network transport](NETWORK.md).

## Session stages

1. Connect to a configured or discovered endpoint. Discovery is only a hint.
2. Authenticate the peer against an explicitly supplied public certificate for the development test. Future pairing trust and revocation are not implemented. Reject an unknown certificate before processing any input request.
3. Exchange `Hello` with major/minor protocol versions and stable peer identity. Reject incompatible major versions and higher minor versions unsupported by this build. Future minor-version compatibility rules must be designed before the wire format is frozen.
4. The controller checks the peer's `debug_input_sink` capability, sends `SessionStart`, and waits for `SessionAccept` before starting capture. Display negotiation and injection policy are future work.
5. Exchange heartbeat and input events while active. End on explicit leave, timeout, disconnect, permission loss, or local user override.

The authenticated transport must bind the claimed node identity to its certificate or key. `Hello.node` alone is never proof of identity. Pairing verification is a separate protocol to be designed. Authentication and encryption are required before control traffic; there is no plaintext mode.

## Initial message concepts

The active debug wire set has `Hello`, `SessionStart`, `SessionAccept`, `Input`, `Heartbeat`, `HeartbeatAck`, `SessionEnd`, and `Error`. `ControlEnter`, `ControlLeave`, and `DisplayInfo` remain semantic scaffolding. `Input` currently covers **relative** pointer movement, buttons, scroll, and physical key press/release. Absolute pointer position requires a separate display/region identifier and coordinate mapping; it is not represented yet. Wayland capture represents key codes as `KeyCode::LinuxEvdev`; this records the source code domain and is not a cross-platform wire mapping. Future pairing and clipboard messages need separate design. No screen pixels, shell commands, or arbitrary remote execution messages belong in this protocol.

The current wire format is a four-byte big-endian length and fixed-integer `bincode` payload capped at 512 bytes. The decoder rejects malformed input, trailing data and invalid event values. The DebugSink limits events to 20,000 per second. Future revisions still need session IDs, replay/sequence handling, frozen unknown-field behavior and cross-platform input mapping. Coordinate precision, held-state recovery, display scaling and multi-monitor identifiers require interoperability tests before freezing the wire format. TLS 1.3 over TCP is the development transport; QUIC remains a candidate if multiple independent streams become useful.

The development connection sends heartbeat/ack every second and times out after four seconds without a response. A disconnected Wayland sender releases local portal capture. Reconnection currently requires a new authenticated command. Transport may use any reachable private-network IP or hostname; LAN multicast discovery is optional and must never gate manual pairing.
