# Security boundaries

UKVM eventually controls a remote keyboard and pointer. Treat its network protocol and discovery traffic as hostile input. The current listener is a DebugSink: it validates and counts events but cannot inject input. It uses mutual TLS 1.3 and manually exchanged pinned public certificates; pairing and revocation are not yet implemented. The experimental Wayland capture runs only after portal permission, has a 60-second safety limit, releases after five active seconds or Esc/Ctrl-C, and reports event counts without printing key codes or text. A portal `Release` request may suggest a cursor position inside the local zone, but the compositor is not required to honor it. A future Wayland injector must open a user-authorized RemoteDesktop session, bind only granted EI devices, stop on session closure/disconnection, and release any held keys or buttons. Portal properties alone do not authorize injection.

## Trust and authorization

- Discovery provides endpoint candidates, never trust. A discovered name or address can be spoofed.
- Pairing must require explicit approval on the machines involved and an out-of-band verification step, such as a short code derived from the authenticated handshake. The exact protocol is not yet selected. No auto-trust or default shared secret.
- Persist stable peer identities and trust decisions separately from the human-readable config. Use OS protected user storage or a strictly permissioned per-user data directory, with secret storage reviewed per OS. Never log private keys, verification secrets, or raw keystrokes.
- Accept input only on a mutually authenticated encrypted connection from a trusted peer with an active authorized control session. Bind all control messages to that session and apply configured role and current local permission checks.
- Provide local user override and immediate disconnect/revocation. Release held keys and buttons when control ends or health checks fail. Remote peers must have no command execution facility.

## Network and parser boundary

The development transport uses `rustls` TLS 1.3 over TCP with mutual pinned-certificate verification. It binds the claimed node ID to the TLS certificate. The private key is stored outside TOML under the user data directory, mode 0600 on Unix; the parent directory is mode 0700. Manual certificate exchange must use an authenticated channel and fingerprint check. There is no trust-store UI, rotation, revocation or automatic pairing. The listener defaults to loopback; an operator must explicitly bind a LAN address. The current listener serves one peer at a time. See [network transport](NETWORK.md).

The parser bounds frames to 512 bytes before body read, validates version, identifiers, strings, event ranges and rate, and rejects unexpected session messages. The sender queue holds at most 256 events; pointer motion may be dropped under backpressure, while failed delivery of stateful events stops capture. TLS reads/writes and heartbeats have timeouts. The platform loop polls connection health and releases active capture on disconnect. Localhost tests cover certificate rejection and peer disconnect, but a live peer-loss-during-capture test is still required. Continue fuzzing and hostile-input testing before real injection. Never disable certificate verification or introduce custom cryptography.

## OS permission boundary

Input capture and injection must use OS sanctioned interfaces and surface permission failures. On Wayland, portal approval belongs to the compositor and user; UKVM must not bypass it with XWayland, privileged `/dev/input` access, compositor patches, or fake global hooks. On macOS, respect Accessibility/Input Monitoring permissions. On Windows, respect session and integrity boundaries. Do not run the network-facing node with elevated privileges merely to make input work.

## Reporting issues

Until a private security contact is established, avoid posting exploit details or secrets in public issues. Maintainers should add a dedicated security reporting channel before accepting network or input injection code.
