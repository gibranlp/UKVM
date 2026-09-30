# Persistent foreground runtime — Milestone 13

`ukvm run` (or `ukvm run --verbose`) runs both node capabilities continuously in the logged-in graphical session. There are no new permanent server/client roles. The existing identity, pinned peers, permissions and layout are reused; pairing remains a separately invoked explicit ceremony. Diagnostic `listen`, `control`, `connect` and platform commands remain available.

M13 code and automated validation are separate from physical acceptance. M12 at `c250dac28d9daf3d981b45cd8ccbf9eaf2dc460b` proved spectrum RIGHT → io LEFT → spectrum using diagnostic commands. **No M13 physical round trip, restart/network recovery or long-duration run has yet been recorded.**

## Ownership and focus

`ukvm-runtime` is a reusable library; the CLI parses two options and runs it. One structured Tokio join owns receiver, controller and discovery workers plus SIGINT handling. Shutdown broadcasts cancellation and awaits every worker's cleanup. The platform crate continues to own portal, EI, geometry and native input operations. Peer availability explicitly tracks dormant, connecting, authenticated, ready, active, unavailable and stopped states; verbose output records transitions.

The focus coordinator uses these explicit states:

| State | Meaning |
| --- | --- |
| Local | Local input; no enabled runtime capture barrier |
| Armed | A pinned, authorized, EI-ready outgoing peer permits a local barrier |
| Controlling | Local capture forwards to that peer |
| Yielding | Incoming entry requests an idle outgoing barrier to close |
| Reserved | Idle local capture is closed; incoming entry owns the reservation |
| Controlled | Incoming authenticated input may be injected |
| Disarming | Incoming entry was cancelled; old local capture must still close |

Outgoing preparation must complete mTLS, matching Hello versions and remote session authorization before Armed. Failed discovery, connection, certificate verification, permission, protocol or portal preparation never creates InputCapture. Capture polls sender health and current permission every 100/250 ms respectively. An idle barrier can yield to incoming entry; injection waits for its Close. A competing active session is rejected. Idle accepted network relationships do not hold active input focus. Only one incoming network connection and one active input relationship are supported at a time.

ReturnControl, the emergency chord and disconnect all end the outgoing relationship, Release/Disable/Close InputCapture, and permit another attempt. SIGINT registers before startup is announced; cancellation during a connection or consent wait closes that attempt. Before activation, setup waits observe peer health/shutdown and have bounded deadlines. Each capture owns a dedicated portal bus connection, which is explicitly closed if consent/session creation is cancelled before its handle arrives. Active capture is allowed to run its release path rather than being cancelled by dropping its future. Sender tasks own both TLS halves and abort on drop; normal finish joins them. The dedicated injector thread closes its held input/portal state and is joined when destroyed.

## Connections and discovery

The listener binds `[network].listen`. Runtime DNS-SD advertises an authenticated control endpoint with `mode=control`, not an open pairing endpoint. Discovery repeatedly browses for four seconds, then pauses six seconds. Results remain bounded, untrusted connection hints. Only the configured layout ID is selected, and every hinted or configured address must pass the original pinned mTLS certificate and Hello ID checks. Runtime hints never write trust, layout or permission records. A recovered peer at a changed IP needs no manual address update while discovery can find it.

Failed outgoing attempts retry after 1, 2, 4, 8, then 16 seconds; successful session completion resets this backoff. Successful returns wait 250 ms before preparing again. Connection/TLS/Hello and session readiness have the existing network deadlines. No input state is replayed after reconnect. DNS-SD failure retries independently and leaves configured-address connectivity available. Stale/spoofed hints fail authentication and cannot substitute another identity.

EOF or write failure restores local control when observed. Silent network interruption uses the existing four-second heartbeat health deadline, then the capture health poll and portal cleanup. “Immediate” restoration means after loss is detected; reachability cannot be known instantaneously. Individual failures do not terminate the foreground runtime. Permission removal/trust-store errors fail closed; permissions are reread during a relationship.

## Portal/EI lifetime

The receiver opens a RemoteDesktop proxy lazily for an authorized session. Normal portal consent and granted EI devices must be ready before RemoteSessionAccept. The existing actor retains a legitimate grant between sessions, releasing held keys/buttons and resetting virtual pointer entry state on each park. It does not repeatedly ask while that grant remains valid. Backend invalidation closes the old grant; a subsequent attempt follows normal authorization again. Runtime shutdown closes the retained grant. No restore token is saved; restart requires fresh consent.

Outgoing InputCapture sessions are recreated after return/failure; an idle barrier is refreshed after one hour. This is separate from receiver consent reuse. Emergency Ctrl+Alt+Shift+Escape remains required, with keyboard capture permission checked before arming. Input Protocol **1.2** and Pairing Protocol **2** are unchanged.

## Automated evidence

All five workspace checks passed locally on Linux after the final cleanup changes. The workspace suite passed 116 tests, with four opt-in multicast tests skipped:

```sh
cargo fmt --all -- --check
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace
```

Focused tests exercise foreground startup/SIGINT/listener release, unavailable peers and permission denial before portal construction or arming, spoofed discovery identity rejection, bounded retry/reset and cancellable waits, peer restart at a new endpoint without a trust edit, ten simulated grant-reusing returns, ten returns through real localhost pinned mTLS plus active disconnect with simulated held keys/buttons, exclusive focus, cancellation during inbound entry and capture-close ordering, and owned sender task cancellation. Existing portal geometry, emergency chord, malformed input, version compatibility, transport health and injector cleanup tests remain. Injector grant/held-state tests use a simulated backend; they do not establish compositor behavior.

The multicast process tests remain opt-in in the workspace suite. The M13 two-PC physical acceptance below is pending.

## Physical acceptance procedure

After the implementation commit has been pushed, prepare each repository/terminal:

```sh
git pull --ff-only origin main
cargo build --workspace
export PATH="$PWD/target/debug:$PATH"
```

The PATH setting selects the freshly built executable for that terminal; a stale executable will not have `run`.

On io (`192.168.1.40`):

```sh
ukvm run --verbose
```

On spectrum (`192.168.1.21`):

```sh
ukvm run --verbose
```

The existing spectrum-right/io-left link and io's explicit `control-this-node` grant to spectrum must remain configured. io still needs the M12 consent-capable portal draft; the packaged Hyprland portal has no RemoteDesktop. Approve the normal first incoming portal consent. Wait for spectrum's authenticated readiness and InputCapture edge-enabled event.

Record each result independently:

1. Complete at least ten spectrum → io → spectrum round trips without restarting either process. Verify pointer, clicks, scroll, keyboard and modifiers.
2. Use Ctrl+Alt+Shift+Escape during control, then start another session by crossing the edge.
3. Stop io with Ctrl+C while spectrum keeps running. Cross spectrum's edge; local input must remain usable.
4. Restart only io, approve fresh normal consent, and switch again without re-pairing or restarting spectrum.
5. Temporarily interrupt io's network using the user's normal network controls. Check local usability, restore it, and switch without restarting either process.
6. Disconnect io during active control with a modifier/button held. Check spectrum's Release/Close and restored local control, plus io's held-state cleanup where its EI connection remains valid. Reconnect and switch again.
7. Leave both idle for several minutes, then switch. If practical, record a 30–60 minute run with actual duration and observed failures.
8. Stop both with Ctrl+C; verify no held input and clean exit.

Do not report any item as passed until physically observed. Preserve logs of lifecycle/cleanup errors; verbose mode does not print individual input events.

## Current limits

Linux Wayland only, with **at most one outgoing layout edge per node**. More edges are rejected at startup rather than silently ignored. Ambiguous outer edges require further mapping support; no multi-monitor UI is added. Node preferences/layout are read at startup; peer policy is checked live. One active relationship is supported; there is no multi-hop handoff or fair scheduling across many peers. Virtual EI pointer coordinates remain requested state, not authoritative compositor cursor readback.

No services, autostart, IPC/status daemon, GUI, tray, clipboard, file transfer, new backends or relays are included. Other compositors, other OSes and M13 sustained physical operation remain untested. SIGINT is the supported clean stop; crashes/SIGKILL rely on connection-loss cleanup at the surviving node. Portal calls can take their backend timeouts during setup/cleanup.
