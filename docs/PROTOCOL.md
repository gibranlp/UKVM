# Input protocol sketch (version 1.2)

The input protocol is a provisional development format. Each TLS application frame has a four-byte big-endian length and a fixed-integer `bincode` payload, capped at 512 bytes before allocation. Both peers must use the exact major and minor version. The protocol is authenticated by pinned mutual TLS; `Hello.node_id` must match the authenticated certificate fingerprint. Pairing is a **separate version 2 protocol** on port 24801 and is unchanged by this revision. Discovery is an untrusted address hint.

## Session types and lifecycle

`Hello` advertises controller, DebugSink and remote-input capabilities. `SessionStart`/`SessionAccept` request only the non-injecting DebugSink. `RemoteSessionStart`/`RemoteSessionAccept` request a locally authorized injector. The controlled node checks pinned trust and `control-this-node` permission, obtains portal consent, establishes EI devices, and only then accepts. The controller must not begin InputCapture before this acceptance. Heartbeat/ack runs every second with a four-second health timeout. Disconnect or session failure releases local capture and remote held inputs where the EI connection remains usable.

After InputCapture activates, the controller sends `RemoteEntry(EdgePosition)` before any `Input` message. `EdgePosition` contains one of left/right/top/bottom and a finite normalized fraction in `[0,1]`, measured from the top or left of that edge. The entry edge comes from the configured layout link; the fraction comes from the controller's portal activation position along its accepted barrier. The controlled node places its EI absolute pointer inside the corresponding outer edge of an advertised EI region. A remote session rejects input sent before `RemoteEntry`.

When the controlled node detects an intentional outward crossing of that same outer edge, it stops consuming input, releases held keys/buttons, closes its injector session, and sends `ReturnControl(EdgePosition)`. The controller accepts this message only from its authenticated active peer and only for the expected return edge. It stops forwarding, requests InputCapture `Release` with the returned fraction along the originating barrier, then closes the local capture session. Late input after return is ignored by the receiver. There is no permanent server/client product role; these are per-session directions.

`SessionEnd` remains the normal explicit end when no natural return occurs. Emergency release, Ctrl+C, disconnect, permission loss and backend failure also end the session. `ControlEnter`, `ControlLeave` and `DisplayInfo` remain semantic scaffolding, not active wire messages. There is no remote shell, screen pixel stream, clipboard or arbitrary command message.

## Why version 1.2

Version 1.2 adds the two return-edge control messages and their ordered session semantics. The `bincode` enum layout changes, so 1.1 and 1.2 peers are intentionally incompatible and rejected at `Hello`. Both computers must pull and build the same revision. Version 1.2 does not alter Pairing Protocol 2.

## Input model limits

`Input` carries relative pointer deltas, pointer buttons, scroll and physical key press/release. The experimental Linux Wayland receiver turns those deltas into EI absolute pointer requests after `RemoteEntry`; absolute coordinates are internal to that backend and are not a cross-platform input wire type. The portal does not return authoritative compositor cursor coordinates, so this virtual pointer position assumes the compositor accepts the absolute requests. Pointer constraints and compositor transforms need physical testing.

Keys remain `KeyCode::LinuxEvdev` with an explicit source code domain. This first Linux-to-Linux path assumes compatible keyboard layouts; it is not a Windows/macOS mapping. Button codes, scroll units, scaling, keyboard repeat and cross-platform layout translation remain provisional. The decoder rejects malformed messages, out-of-range positions, non-finite or oversized input deltas, invalid key/button codes and unexpected messages for the current session state. Input rate and queue sizes are bounded. No claimed one-way latency uses unsynchronized host clocks.

## Persistent runtime compatibility

M13 uses the existing 1.2 Hello, RemoteSessionStart/Accept, RemoteEntry, ReturnControl, SessionEnd and heartbeats on a new authenticated connection for each outgoing relationship. Idle preparation follows the same readiness exchange before capture. Runtime entry may await local idle-capture cleanup before consuming input. No input wire variants or encodings change; Input Protocol 1.2 and Pairing Protocol 2 remain unchanged. DNS-SD adds an untrusted endpoint-mode TXT hint, outside either wire protocol.
