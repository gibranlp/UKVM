# Plain-text clipboard sharing (M19)

M19 implements opt-in, bidirectional text sharing in the Linux Wayland runtime.
Spectrum ↔ Hellgen physical acceptance remains pending. Windows/macOS backends,
images, files, HTML, primary selection and arbitrary MIME transfer are not implemented.
The platform-independent protocol, `ukvm-core::clipboard::ClipboardState` ordering/deduplication engine and `ClipboardBackend` seam support future native backends.

## Enable and revoke

On each computer, explicitly allow its trusted counterpart:

```sh
ukvm peer allow OTHER_PEER share-clipboard --json
ukvm status --json
```

Both nodes must run M19 and allow `connect` and `share-clipboard`. Sharing defaults
off for new and existing peers. It grants both send and receive for that peer;
`control-this-node` and desktop input consent remain entirely independent.
No placement or pointer focus is required. Connected authorized peers receive
local copies directly; received copies are not relayed to other peers.

```sh
ukvm peer deny OTHER_PEER share-clipboard --json
```

Clipboard edits are live: `apply_required:false`, `apply_command:null`. No service
restart is needed. Sends and final clipboard writes hold a shared trust-edit lock;
revocation waits for any already authorized bounded write, then blocks subsequent
sends and queued writes. Idle connections close within one policy-monitor tick.
Already copied data in another computer's clipboard/history cannot be recalled.

## Linux backend

The native Rust backend prefers `ext-data-control-v1`, falling back to
`wlr-data-control-v1` when provided by the compositor. It operates on the first
seat's regular clipboard and serves text through owned Wayland sources. It creates
no window, shell command, detached helper, XWayland connection or privileged device access.
Unsupported compositors report clipboard unavailable without affecting input.
`doctor --json` examines only registry capabilities; it never reads a clipboard offer.

References: [ext-data-control specification](https://gitlab.freedesktop.org/wayland/wayland-protocols/-/blob/main/staging/ext-data-control/ext-data-control-v1.xml),
[wlr protocol](https://gitlab.freedesktop.org/wlroots/wlr-protocols/-/blob/master/unstable/wlr-data-control-unstable-v1.xml),
[Wayland Rust bindings](https://github.com/Smithay/wayland-rs).

Event queues, transfer jobs, incoming messages and peer relationships are bounded.
The listener reserves separate limits for 16 input sessions and 64 clipboard peers,
with at most 80 concurrent incoming handshake/session jobs.
Transfers have deadlines; failed backend initialization retries after ten seconds.
Owned-source markers and selection generations suppress echoed/stale readbacks.
No clipboard device/read is created while all clipboard permissions are off.
Library dependencies are locked `wayland-client`, `wayland-protocols` and
`wayland-protocols-wlr`; no external clipboard executable/package is needed.
`wayland-server` and server protocol bindings are test-only dependencies.

## Transport and lifecycle

Clipboard Protocol 1 uses ALPN `ukvm-clipboard/1` on the existing control listener,
existing pinned TLS 1.3 certificates and authenticated cryptographic node IDs.
An explicit Hello negotiates version, UTF-8 text MIME and 65,536-byte limit.
Input Protocol 1.3 and Pairing Protocol 2 are unchanged; older peers retain input
compatibility and fail closed on clipboard negotiation. Discovery carries no clipboard data.

Text frames contain authenticated origin, random runtime epoch, monotonic logical
generation, byte length and UTF-8 text. Empty text is valid. Clearing the regular
clipboard sends empty text; non-text offers are ignored. Oversized/invalid UTF-8
offers produce safe diagnostics. Four-byte big-endian length prefixes bound the
bincode payload to 65,792 bytes; oversized, trailing, malformed, mismatched-origin
and unexpected messages are rejected before writing a clipboard. Hello must match
the supported version/type/limit. New content types require explicit negotiation.

Only the node with the lexically smaller authenticated ID initiates the clipboard
connection, preventing duplicate links. Reconnect uses bounded backoff and current
local text when available. Content comparison and per-origin epoch/generation
tracking avoid duplicate writes and retransmission; logical ordering with identity
tie-breaking resolves simultaneous updates without relaying remote content.
Runtime shutdown closes sources, sockets and transfer jobs cooperatively.
Permission storage survives restart/login/reboot; copied text and deduplication
state are memory-only. This is synchronization, not durable clipboard history.
The desktop's normal clipboard/history manager may retain values separately.

Safe frontend state is documented in [FRONTEND_CONTRACT.md](FRONTEND_CONTRACT.md).
Neither Debug diagnostics nor status/JSON exposes text or content hashes.

## Acceptance

Install the M19 package on both existing paired machines. Restart each user service,
then explicitly enable clipboard permission for the other peer on both computers.
Require `doctor --json` clipboard support and `status --json` one connected clipboard
peer. Copy `hello` on Spectrum, cross to Hellgen and paste into a text editor.
Copy `world` on Hellgen, return and paste on Spectrum. Repeat with Unicode, multiline
text and an empty value; verify no repeated clipboard changes. Revoke on either
node and verify a fresh copy does not synchronize, while input control still works.
Re-enable and repeat after network reconnect, service restart, logout/login and
reboot. No re-pairing or reset of desktop portal authorization is required.
