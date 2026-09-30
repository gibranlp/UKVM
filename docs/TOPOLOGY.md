# Multi-node runtime and 2D topology — Milestone 14

M13 is physically validated on spectrum/io, including emergency release and recovery. M14 adds multiple independently available peers and coordinate-based focus traversal. M14 automated tests use real localhost pinned mTLS with simulated injection; multi-node portal/EI operation has not yet been physically tested.

## Configuration

Each node defines its own relative topology. Local is implicitly `(0,0)`. Positive x is right; positive y is bottom. M14 accepts `(±1,0)`, `(±2,0)`, `(0,±1)` and `(0,±2)`. Coordinates and adjacency are modeled independently of screen pixels and node labels; further shapes can be supported later. A distance-two position requires the corresponding distance-one position to be configured. An offline intermediate is never skipped.

Use the CLI to resolve an existing trusted alias into its pinned Node ID:

```sh
ukvm layout place 1 0 io
ukvm layout place 2 0 laptop
ukvm layout place -1 0 work
ukvm layout place -2 0 macbook
ukvm layout place 0 -1 server
ukvm layout place 0 -2 top-laptop
ukvm layout place 0 1 test
ukvm layout place 0 2 bottom-laptop
ukvm layout
ukvm config check
```

Only place peers actually paired/imported and supported by the current backend. These names are examples, not built-in positions or supported new OS backends. `place` rejects invalid/conflicting placements without writing a partial config. `remove-position X Y` removes a position; remove a distance-two position before its intermediate. Removing a trusted peer also removes its placements and any now-orphaned outer placement, but does not remove another peer's trust record.

Persisted preferences follow the existing table-array convention:

```toml
[node]
name = "spectrum"
role = "both"

[network]
listen = "0.0.0.0:24800"

[[layout.nodes]]
x = 1
y = 0
peer_id = "<io's full 64-character certificate fingerprint>"

[[layout.nodes]]
x = 2
y = 0
peer_id = "<laptop's full 64-character certificate fingerprint>"
```

Replace the illustrative fingerprints with real pinned IDs; aliases/IPs are not valid identities in TOML. Existing `[[layout.links]]` remain supported as distance-one placements. Do not duplicate a coordinate across links and nodes. `place` can replace a legacy link at that coordinate. Duplicate coordinates/IDs, local placement, diagonals, distance greater than two, gaps and unknown fields are rejected. Runtime also rejects local identity as a remote node. Missing trust is a startup error; denied/offline/incompatible peers remain unavailable independently.

The diagnostic `control EDGE` still addresses only the adjacent node; it also recognizes coordinate placements. Guided setup remains a distance-one editor. Normal multi-node use is `ukvm run` on every node.

## Focus routing and authorization

The physical source of a focus path keeps InputCapture and chooses destinations using its own topology. It connects directly to each configured destination through pinned mTLS. A receiver reports a cardinal boundary and edge fraction, never a destination identity/address. The source computes `current coordinate + direction`, checks readiness and selects the pinned adjacent peer. There is no network relay or implicit delegation of a receiver's permission.

For spectrum `(0,0)`, io `(1,0)`, laptop `(2,0)`, right crossings move spectrum → io → laptop. Left crossings traverse laptop → io → spectrum. The origin keeps capture across remote-to-remote handoffs and releases it on final local return. Each receiver releases held keys/buttons before sending ReturnControl; the source transfers still-physically-held keys/buttons to the next authenticated recipient. Keyboard/button delivery failure ends the path; pointer motion can be dropped under bounded backpressure.

Every receiver must explicitly trust and grant `control-this-node` to the **physical source**, including a distance-two receiver. For example, laptop must authorize spectrum for a path physically originating on spectrum; trusting only io does not authorize spectrum. All receivers require normal RemoteDesktop/EI consent. To originate an equivalent path from laptop or io, pair/authorize that source separately and configure coordinates relative to it. No node has a permanent server/controller role. Opposing active paths are rejected, not preempted.

Input Protocol **1.3** adds bounded `TopologyEntry { entry, exits }` and `TopologyExits` updates. Exits contain at most four distinct cardinal directions. Diagnostic RemoteEntry retains its single-return-edge behavior. Runtime ReturnControl may select only an exit advertised by the physical source. Peers must use matching 1.3 builds; 1.2 fails closed during Hello. Pairing Protocol **2** is unchanged; no re-pairing is required.

## Availability, consent and cleanup

Each configured peer has an owned preparation worker, availability state and bounded 1/2/4/8/16-second retry sequence. Discovery hints remain ephemeral and must pass the existing certificate/Hello identity checks. No hint changes trust or permissions. An unavailable peer neither blocks other workers nor enables its local edge. Up to four ready adjacent barriers share one InputCapture session. Idle readiness changes rebuild barriers; during active control, unused peer changes only refresh advertised remote exits. A farther offline peer clamps movement at the current receiver's boundary while other paths remain usable.

A worker maintains one ready connection per peer, including an idle return standby while that peer is actively controlled. This avoids reconnecting the intermediate from scratch during ordinary quick reverse traversal. A source owns at most eight ready senders plus one active sender. The listener serves up to 16 simultaneous preparation/session connections, with bounded handshake/consent deadlines, and rejects excess sockets. These idle relationships do not own input focus. If a selected adjacent node has become unavailable or busy by the time of handoff, cleanup restores the physical source rather than capturing indefinitely or skipping a node.

One shared permissioned RemoteDesktop/EI grant is initialized under a serialized initialization lock, then parked between entered sessions. Ready connections never begin/park another connection's active injection session. Focus reservation waits for idle local capture to close before entry. Only the entered lease may release held input. A failed grant is replaced after the former owner's cleanup; replacement follows normal consent. Entry can only resume an already prepared Wayland grant and fails promptly if it was invalidated, rather than requesting a new consent picker while source input is captured. No restore token is stored. Runtime shutdown cancels and drains all incoming/preparation workers, finishes senders, releases capture and closes the shared grant/actor.

M13 emergency release remains local to the physical source. At either remote level, Ctrl+Alt+Shift+Escape prioritizes SessionEnd over queued input, releases capture, and suppresses the **launching local edge** until the observed local pointer is at least 32 logical pixels inward. Other ready edges remain usable. Time and reconnects do not clear the guard. Idle observation currently uses Hyprland's read-only cursor IPC; unavailable observation leaves the edge suppressed. Normal ReturnControl and disconnect do not latch this guard. Silent network loss still uses the existing four-second health deadline plus portal cleanup; EOF/write loss is handled when observed.

## Automated and physical evidence

All five required workspace checks passed: formatting, check, test, Clippy with warnings denied, and build. The suite passed 142 tests; four opt-in multicast tests and the live Hyprland observer test were skipped.

Focused tests cover all eight positions, two-level topology roundtrip parsing, duplicate/invalid coordinates and gaps, atomic placement/removal, legacy layouts, malformed/oversized topology messages, all four EI exit directions, unavailable exits and refresh, shared-grant exclusive ownership, and readiness cleanup that cannot release an active owner's held state. Real localhost pinned TLS tests cover spectrum → io → laptop → io → spectrum, modifier/button handoff cleanup, independent unavailable paths, warm return standby, cancellation/draining, remote exit refresh, active disconnect and emergency release from the second node. Existing M13 denial, identity conflict, peer restart/recovery, repeated sessions, portal reuse and hysteresis tests remain.

After the commit is pushed, pull/build on every test node:

```sh
git pull --ff-only origin main
cargo build --workspace
export PATH="$PWD/target/debug:$PATH"
```

Prepare explicit pairing/trust and control permissions first; see [pairing](PAIRING.md). With the existing io grant to spectrum, configure spectrum:

```sh
ukvm layout place 1 0 io
ukvm layout place 2 0 laptop
ukvm config check
```

On laptop, after verified pairing with spectrum:

```sh
ukvm peer allow spectrum control-this-node
```

Start receivers first (io, laptop), then spectrum, using only:

```sh
ukvm run --verbose
```

Approve normal portal consent. Wait for each peer's coordinate/Ready log. Test and record separately:

1. Ten spectrum → io → laptop → io → spectrum cycles. Test motion, clicks, scroll, keys, modifiers and a held modifier/button across both handoffs. Verify cleanup on the previous recipient and correct state on the next.
2. Repeat with a second adjacent direction, preferably a top/bottom peer; validate the correct node and normalized entry fraction. Test a reverse-origin path from io or laptop with its own source permissions/relative layout.
3. Stop laptop: spectrum → io and natural return must still work; io's right edge must stay local to io. Restart laptop, grant fresh normal consent, and reach it without restarting spectrum/io or re-pairing.
4. Stop io: spectrum's right edge must stay usable locally and must not skip to laptop. An independently ready top/left/bottom path must continue working. Restart io and verify both right levels recover.
5. Disconnect the active receiver's network with a key/button held. Check physical-source Release/Close and held-state cleanup where EI remains available. Restore the network and switch again without runtime restart.
6. Emergency-release at laptop. Tangential/near-edge movement on spectrum must not relaunch the right path. Move at least 32 logical pixels inward, then cross intentionally again. Confirm another ready local direction still works while right is suppressed. Repeat after natural return and verify no emergency latch.
7. Deny source permission on one receiver; its path must fail closed while other authorized paths work. Attempt competing physical input sources and verify one active recipient session.
8. Idle for several minutes, switch again, and finish with Ctrl+C on all nodes. Record actual endurance duration if tested; no M14 physical/endurance result is claimed yet.

## Limits

Linux Wayland with permissioned portal/EI only. M14 supports eight cardinal positions, not diagonal grids, cycles, arbitrary distance, monitor mapping UI or network relaying. Multi-segment ambiguous local outer edges still fail clearly; existing display geometry restrictions remain. EI virtual position is requested state rather than compositor cursor readback. If a node is busy or has no ready reverse standby, a rapid reverse handoff may restore the physical source instead of waiting with input captured. All topology/preferences are loaded at startup; permissions and readiness are checked live.

No services, autostart, GUI/TUI, clipboard, file transfer, new OS backends or SpectrumOS integration are added. Milestone 15 is not started.
