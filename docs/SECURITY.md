# Security boundaries

UKVM's network protocol and discovery traffic are hostile input. The DebugSink validates and counts events but cannot inject input. The separate experimental `listen --remote` mode creates an injector only if RemoteDesktop v2 is available and accepts only a pinned peer with local `control-this-node` permission. Ordinary sessions use mutual TLS 1.3 with pinned public certificates. Pairing establishes pins but never input permission. Wayland capture runs only after portal permission, has a 60-second waiting limit, and reports counts without printing key codes or text. Debug capture releases after five active seconds or Esc/Ctrl-C; remote capture has no active timeout and ends on natural return, Ctrl+C, Ctrl+Alt+Shift+Escape, disconnect, permission loss or backend failure. A portal `Release` request may suggest a local cursor position that the compositor can ignore. The EI sender tracks held keys/buttons and releases them on return, normal end, peer loss, permission revocation, and backend failure where the EI connection remains available. Natural return, pointer, buttons, scroll, keyboard, and local restoration have been physically observed between spectrum and io. In a controlled abrupt sender-stop test, Shift and a mouse button were held remotely; io reported an unexpected TLS EOF, and the user confirmed that local clicking and typing worked on both PCs afterward with no held state.

The first authorized remote-control session asks for local RemoteDesktop consent. The listener retains that permissioned portal/EI session while it runs, releases held input between network sessions, and closes the portal session on local shutdown or permission revocation. M18 requests official persistence and stores a returned single-use restore token privately; otherwise restarting requires normal consent. An idle listener retains portal input authority; it remains an explicit local choice, and UKVM checks authenticated peer permission again for every network session. A second physical session on the same listener opened without another picker and completed a natural return.

## Trust and authorization

- Discovery provides endpoint candidates, never trust. A discovered name, address or Node ID hint can be spoofed. Guided setup classifies these as hints, requires pinned mTLS before updating a trusted peer's address, and otherwise keeps the existing pinned record unchanged.
- Pairing requires explicit approval on both machines and comparison of a short value bound to a fresh encrypted exchange and both public identities. It creates only the existing pinned trust records. No auto-trust, default shared secret or input-permission grant. See [pairing threat model and protocol](PAIRING.md).
- Interrupted pairing commits keep a nonsecret, time-limited record of mutual confirmation. Recovery requires a fresh authenticated exchange with the exact original certificate and matching transaction record on both machines. An acknowledgement loss leaves an explicit uncertain state; it never silently substitutes a peer identity or resets permissions. Explicit peer removal invalidates recovery receipts for that identity.
- Persist stable peer identities and trust decisions separately from the human-readable config. Use OS protected user storage or a strictly permissioned per-user data directory, with secret storage reviewed per OS. Never log private keys, verification secrets, or raw keystrokes.
- Accept input only on a mutually authenticated encrypted connection from a trusted peer with an active authorized control session. Bind all control messages to that session and apply configured role and current local permission checks.
- Provide local user override and immediate disconnect/revocation. Release held keys and buttons when control ends or health checks fail. Remote peers must have no command execution facility.

## Network and parser boundary

The development transport uses `rustls` TLS 1.3 over TCP with mutual pinned-certificate verification. It binds the claimed node ID to the TLS certificate. The private key is stored outside TOML under the user data directory, mode 0600 on Unix; the parent directory is mode 0700. Manual certificate exchange remains available and must use an authenticated channel and full fingerprint check. `ukvm peer add` confirms the fingerprint before storing a pinned certificate and alias in per-user state. Pairing writes the same records after two-sided verification. The alias and address are never authentication factors. New peers may connect, but DebugSink and control-this-node permissions default to deny. `ukvm peer allow` is explicit; granting debug input does not grant remote control. Automatic certificate rotation does not exist. Temporary mDNS discovery advertises pairing endpoints only; names, addresses, ports and node ID hints from it are untrusted. It neither writes trust nor grants permissions. See [discovery](DISCOVERY.md). The configured listener uses the configured LAN bind address and serves one connection at a time. The legacy `--peer-cert` listener keeps its direct single-certificate development behavior. See [network transport](NETWORK.md).

The parser bounds frames to 512 bytes before body read, validates version, identifiers, strings, event ranges and rate, and rejects unexpected session messages. The sender queue holds at most 256 events; pointer motion may be dropped under backpressure, while failed delivery of stateful events stops capture. TLS reads/writes and heartbeats have timeouts. The platform loop polls connection health and releases active capture on disconnect. A live DebugSink receiver-termination test returned local control. Debug and remote-input listeners advertise distinct session types and enforce distinct receiver-side permissions. The remote receiver rechecks the local trust store during a session; revocation closes the session and calls injector cleanup. Never disable certificate verification or introduce custom cryptography.

## OS permission boundary

M22's Windows foundation creates state with protected current-user-only ACLs and
checks existing owner/DACLs and objects through handles. Unsafe state, reparse
points, extra hard links and unsupported paths fail closed; ACLs are not repaired
automatically. Identity/trust/recovery reads are bounded, and Identity Debug
redacts private bytes. Windows removal flushes invalidation before deletion so
a retained revoked receipt cannot recreate trust. Native enforcement/durability
acceptance is pending. Session/ownership checks do not implement input or grant
peer permissions. See [Windows foundation](WINDOWS_FOUNDATION.md).

Input capture and injection must use OS sanctioned interfaces and surface permission failures. On Wayland, portal approval belongs to the compositor and user; UKVM must not bypass it with XWayland, privileged `/dev/input` access, compositor patches, or fake global hooks. On macOS, respect Accessibility/Input Monitoring permissions. On Windows, respect session and integrity boundaries. Do not run the network-facing node with elevated privileges merely to make input work.

## Reporting issues

Until a private security contact is established, avoid posting exploit details or secrets in public issues. Maintainers should add a dedicated security reporting channel before accepting network or input injection code.

## Persistent runtime boundary (M13)

`ukvm run` is an explicit foreground remote-input mode, separate from the diagnostic default DebugSink. It does not enable pairing or grant permissions. Runtime discovery advertises a stable public identity hint for as long as the node runs, extending its LAN visibility beyond temporary pairing. The TXT `mode=control` marker prevents current pairing browsing from offering runtime endpoints; the marker remains untrusted. Every outbound connection must match the layout ID and pinned certificate. Runtime hints stay ephemeral; a failed hint cannot overwrite a pin or permission.

Outgoing barriers are enabled only after authenticated remote session acceptance, which follows explicit receiver permission and normal portal/EI authorization. Incoming permission is checked before/after portal consent, at entry, and every 250 ms during operation; outgoing connect permission is rechecked at the same interval during capture. Trust-store read/validation failures deny control. Active focus is exclusive; an incoming entry waits for idle local capture to close, and cannot interrupt an active outgoing relationship. Colliding active entries fail closed and clean up.

SIGINT cancels workers cooperatively, releases local capture and held injected state, ends network sessions and closes the retained portal/EI grant. TLS sender tasks are owned and aborted on cancellation; the injector thread is joined on destruction. EOF/write failures restore control when observed; a silent partition can require the existing four-second heartbeat deadline plus capture polling/portal cleanup. No network system can prove instantaneous reachability at the exact edge crossing. There is no replay of held state after reconnect. Runtime restart restores remembered authorization only through the official portal; absent/rejected credentials require normal consent. The user has physically confirmed normal runtime switching and receiver/network/active-disconnect recovery; M13 including emergency release is physically validated.

Emergency release latches a rearm guard before cleanup and prioritizes SessionEnd over queued input. Local capture stays disabled until a local pointer sample is at least 32 logical pixels inward from the launching edge. The platform uses only Hyprland’s fixed read-only cursorpos IPC query, with bounded response size/time; it does not inject input, move the pointer or alter portal authority. Missing/invalid observation fails closed with the edge disarmed. Remote virtual coordinates and suggested portal release positions cannot clear the guard.

## Multi-node topology boundary (M14)

M22 shares pinned connection preparation, ephemeral discovery hints, independent
readiness workers and source-owned adjacency checks in `ukvm-runtime::peers`.
The Wayland runtime composes that state with its existing focus and portal/EI
authority. A live sender alone cannot satisfy readiness after outbound policy
revocation or a trust-store read failure. Stop requests and lost cancellation
senders drain ready connections even without the separate atomic shutdown flag.
This refactor changes no authentication, receiver permission, consent or input
protocol contract and does not enable a Windows/macOS injector.

The physical source chooses each adjacent pinned destination from its local validated coordinate topology. Every receiver directly authenticates that source and independently enforces control-this-node and normal portal/EI consent. There is no permission delegation, remote-selected identity, relay or implicit trust between intermediates. Input Protocol 1.3 topology entry/exit-refresh messages contain at most four distinct cardinal exits, with at most 40 refreshes per second; malformed/out-of-order messages fail closed. A receiver can only report a boundary to the same authenticated source, which checks the advertised exit and readiness.

At most eight ready outbound senders plus one active sender and 16 incoming preparation/session connections are retained. Consent initialization is serialized; inactive connection cleanup cannot park another owner’s input. Held input is released before ReturnControl; still-physically-held state is transferred only to the next authorized recipient. Invalid/busy/unavailable handoff restores the physical source instead of waiting with captured input or skipping nodes. Independent retry does not weaken certificate pinning. The emergency guard suppresses the launching edge independently of other ready edges. Multi-node physical behavior remains pending acceptance.


## Daily runtime and service boundary (M15)

The Linux unit runs in the user's manager and graphical session, never as a system/root input daemon. ConditionUser excludes root; the M15 launcher validates the current graphical login/socket and retries unavailable session/backend readiness without requesting permission. Desktop target/environment integration and explicit enable/start remain local user choices. The wrapper imports only named session/XDG variables, not arbitrary environment or secrets. Portal consent, pins, permissions and input protocols are unchanged. SIGINT/SIGTERM use cooperative cleanup; crash restart never restores held input; M18 restores remembered consent only through the official portal protocol.

A nonblocking kernel flock is acquired before any runtime listener/discovery/portal construction. Private owned runtime directories and a non-symlink regular lock inode are required. The inode is never removed to unlock, avoiding competing locks on replaced inodes; a crash releases ownership automatically. Status is a private bounded, atomically replaced advisory snapshot with public identity/focus/readiness only. It never authorizes input, grants trust or accepts control requests. Stale/malformed snapshots or absent kernel ownership do not claim current focus. Same-user code can edit its own files; this lock is accidental-instance prevention, not isolation from malicious code already running as that user.

## Linux management and frontend boundary (M16)

JSON is a local presentation contract, never a remote control/trust channel. Discovery remains unverified even when its claimed ID matches a pin. Read-only views create no identity or portal grants. Structured pairing reuses Pairing Protocol 2 and requires explicit approval of the exact verified peer and code on both machines; no automatic approval flag exists. Destructive unpair/layout removal requires terminal confirmation or explicit `--yes` after frontend confirmation. Permission edits apply only to the local receiver policy. Saved topology changes use validated managed-service restart, preserving the permanent-inode runtime lock, cooperative shutdown and normal consent. No portal workaround, SpectrumOS dependency or privileged daemon is introduced. See [frontend contract](FRONTEND_CONTRACT.md).

## Remembered portal consent (M18)

Persistent portal credentials remain independent of peer trust and `control-this-node`. UKVM uses official RemoteDesktop/InputCapture v2 persistence only, requesting mode 2 after existing peer authorization checks. Portal approval and granted device masks remain required. Private, bounded, durable records consume single-use credentials before an attempt and atomically store the replacement before using input; failed/cancelled attempts do not replay old tokens. Desktop/API changes require new approval, while the portal/backend validates vendor restore data. Status exposes only safe state. Explicit confirmed reset stops the owned managed runtime, forgets both credentials and leaves peer permissions unchanged. It refuses foreground/unknown owners and does not automatically restart or approve consent. See [PORTAL_PERSISTENCE.md](PORTAL_PERSISTENCE.md); earlier milestone descriptions of fresh consent/no token storage are historical.

## Clipboard boundary (M19)

Clipboard shares UTF-8 text only after separate bilateral `share-clipboard` opt-in
and pinned mutual TLS authentication. New/existing missing permissions default OFF;
pairing, topology, input permission and portal restore tokens do not grant it.
Clipboard Protocol 1 has separate ALPN and capability negotiation; Input Protocol 1.3
is unchanged. Authenticated origin must match the certificate identity. Every send
and final Wayland write rechecks permission under the trust-edit lock, including
queued writes. Revocation closes idle links and prevents subsequent contents access.
Bounded framing, size, queues, transfer deadlines and receive rates reject invalid
data. Received text is never a command or relayed to an unauthorized third peer.
Contents remain memory-only, with redacted Debug and safe status/JSON. Regular OS
clipboard/history remains user-visible and may retain already shared text.
Native data-control uses the compositor's sanctioned clipboard-manager interface,
never input injection APIs, root, XWayland or portal-security bypasses.
See [CLIPBOARD.md](CLIPBOARD.md).

## M20 files

File Protocol 1 runs on a separate pinned mutual-TLS ALPN channel, never discovery. `receive-files` defaults off and requires connection permission independently of clipboard/control. Per-write/finalization shared policy locks and bounded live checks make revocation fail closed. Private staging, descriptor-relative no-follow filesystem operations, portable component validation, exclusive creates, streamed SHA-256 verification and atomic no-replace batch publication prevent path escape and silent collisions. No special files, permissions/ownership/executable bits or command execution are transferred. Payloads/digests are absent from diagnostics and JSON. See FILE_TRANSFER.md for exact bounds, disk checks and interrupted/uncertain receipt behavior.

## Interactive Remote Sessions v1

Screen pixels require separate `view-screen` default-deny permission and legitimate
ScreenCast consent, independently of `control-this-node`, input portal authority,
clipboard and files. Display v2 uses a separate pinned mTLS ALPN channel, bounded
JPEG frames, acknowledgements, timeouts and latest-frame slots. Window input is
Input Protocol 1.4 on the existing input Sender/ExclusiveSink/shared RemoteDesktop
EIS grant, with both viewing and control permission. A private same-user local
socket only leases runtime input focus; it cannot mutate trust, consent or service
state. Revocation/disconnect/release clean held input and focus. No pixels or
keys are logged. The user confirmed end-to-end operation; the performance revision awaits physical comparison. See [remote sessions](REMOTE_SESSIONS.md)
for protocol bounds, coordinate matching, permission checks and testing.

ScreenCast v4+ remembered monitor consent uses a separate private, single-use
credential in the existing authorization store. Only a successful Start can
renew it; the token never grants peer trust or viewing/control permission. First
approval, portal-side revocation/expiry and unsupported backends retain normal
consent. Credential-bearing failures use fixed errors and status contains no
restore data. Confirmed authorization reset now forgets ScreenCast alongside the
two input credentials, after stopping the owned runtime and locking all grants.

Display Protocol 2 adds bounded video-only preferences and numeric monotonic
timing metadata. Neither can grant input/viewing authority. JPEG payloads are
bounded to 8 MiB, decoded pixels to 64 MiB, pending video to one frame per stage,
and outstanding frames to one. The receiver checks image geometry before codec
allocation. Clock estimates are diagnostics only and never authenticate or order
input. Performance reports exclude pixels, input contents and portal tokens.

The native Wayland remote viewer requests surface/seat-scoped shortcut inhibition
only for a focused, connected, authorized interactive control lease. The
compositor decides whether to grant it and may reserve override shortcuts.
Emergency release/focus loss/close/session or permission loss destroys the
inhibitor and ends existing input ownership. An idle/view-only window requests
no inhibition. This is a sanctioned window protocol, not a global capture or
portal bypass; trust, portal consent and receiver-side input permissions remain
unchanged. The guest libwayland backend borrows the window's live display/surface,
owns only its own proxies and queue, and cleans them before event-loop shutdown.
