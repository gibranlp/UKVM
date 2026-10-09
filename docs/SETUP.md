# Guided setup

`ukvm setup [--interface NAME] [--host HOST[:PORT]] [--debug]` is a terminal assistant over existing UKVM state. It uses the persistent node identity, DNS-SD discovery, Pairing Protocol 2, pinned trust store, local permissions and node-level layout. The individual `identity`, `discover`, `pair`, `peer` and `layout` commands remain available. Setup requires an interactive terminal; there is no `--yes` bypass for code comparison.

## M16 local preparation

After identity/name, setup probes the current Wayland portals and shows capability failures without requesting consent. It offers separate, default-no choices to install/update the user service, enable future graphical login startup, and start/restart now. Start uses the same validated `ukvm apply` path and may request normal portal consent. Installation points at the executable used for setup; use a stable installed executable for daily use. Setup reports eligible discovery addresses, the temporary pairing listener requirement, and code comparison on both computers. It can finish local-only preparation without any peer, retaining identity and showing current configuration/service summary. Service failures stop the relevant step visibly; no root, profile hook or development portal trial is installed.

## Pairing flow and security

1. Show the local display name, shortened public Node ID, identity certificate path and eligible interface addresses. Reuse an existing identity. If the active config, state or identity path is under the system temporary directory, show the path and require confirmation before creating identity. Other custom XDG paths are accepted without a temporary-profile warning. Save the default node name on first setup so a later hostname change does not silently rename the node; offer an explicit display-name edit.
2. Browse `_ukvm._tcp.local.` for four seconds, optionally restricted by `--interface`. A failed or empty browse offers another search, manual hostname/IP entry, or cancellation. `--host` skips browsing and uses the same direct Pairing Protocol 2 path as `ukvm pair HOST`.
3. Classify advertised names and IDs as **untrusted hints**. A hint matching a pinned peer is labeled as such, then setup tries pinned mTLS on the advertised control endpoint. A successful TLS identity check permits an optional address-hint update; a failure permits only an explicitly accepted offline configuration of the **existing local trust record**, without adopting the discovered address. A same-name/different-ID claim stops setup with both IDs shown. A candidate claiming the local node's ID is ignored.
4. For an unknown peer, reuse the existing Pairing Protocol 2 initiator and its human code confirmation. The authenticated pairing identity must match a supplied advertised ID hint; a mismatch rejects the ceremony. Existing pairing conflict and commit-recovery rules remain in force. If a matching, incomplete recovery receipt is found, setup points to `ukvm pair status` and the explicit recovery commands before offering a new ceremony. Pairing may leave independently committed trust after interruption; rerun setup or inspect recovery state rather than deleting it.
5. Show permissions stored **on this computer**. Connect is allowed for a newly paired peer; DebugSink and control-this-node default to deny. Changing either input permission requires a separate, directionally worded choice and confirmation. Setup never grants permission on the other computer. Control permission is configuration only; it does not create an injector.
6. Show an existing edge and offer left, right, above or below. Keeping the current edge makes no write. Moving a peer removes its old node-level link. Replacing another peer on an occupied edge requires explicit confirmation. Existing config validation and atomic save apply.
7. Summarize the pinned peer, whether its live identity was verified during this run, local permissions, layout, and probed platform capabilities. A portal-advertised capture interface is not described as a live permission grant. The Wayland capture/injection backends are implemented; advertised interfaces are not consent or physical proof. RemoteDesktop v2 and keyboard/pointer are required for injection readiness. Real control requires permission on the receiving machine and normal portal/EI consent. Apply name/layout edits with `ukvm apply`.

`ukvm setup --debug` prints public fingerprints, path and interface choices, discovery hints, and verified endpoint decisions. It does not print private keys, Noise state, verification secrets or input events. Ctrl+C before pairing confirmation cannot create trust. After a committed pairing, cancellation of permissions or layout does not delete that valid trust. Repeated setup preserves identity, aliases, permissions and unchanged layout.

In an isolated local terminal trial, setup used direct `--host` pairing with a second profile. Both terminals displayed the same code, reciprocal pinned peers were stored with input permissions denied, and setup saved the right-edge link. A repeat run discovered that peer over mDNS, verified it through pinned TLS on the normal listener, preserved the address when its optional update was declined, and kept the existing permissions and right-edge layout.

## Physical acceptance: existing `spectrum` ↔ `io`

Milestone 10 discovery was physically proven between these two PCs. The Milestone 11 already-trusted setup path was physically tested on September 28, 2026 using revision `697af04258712c38d03475176b407c88b7524814`: A discovered `io` at `192.168.1.40:24801`, recognized only an unverified pinned-ID hint from mDNS, verified B's pinned identity over mTLS at `192.168.1.40:24800`, and completed without starting a new pairing ceremony. A retained its certificate fingerprint `fbbd566c8d71897375dc4e829fbc1161d09b2254490fcd8efeffdf2c7e6992bd`; its configured display name changed from hostname fallback `Hell` to `spectrum` by explicit choice. The existing right-edge link, io address hint `192.168.1.40`, and permissions (connect allowed, debug input and control denied) were preserved. `ukvm config check` passed afterward. This proves the physical already-trusted path; the new-peer setup path has local two-profile evidence and remains untested between these physical PCs. Use the **persistent** profiles for the already-trusted test; do not replace either identity with a temporary one. Both machines must build the same pushed revision. On each checkout:

```sh
git pull --ff-only origin main
cargo build -p ukvm
target/debug/ukvm identity
```

On PC A, the identity should be `fbbd566c8d71897375dc4e829fbc1161d09b2254490fcd8efeffdf2c7e6992bd`. If it differs, stop and inspect XDG paths before setup. On PC B, check its expected persistent identity and existing `spectrum` peer with `target/debug/ukvm peer show spectrum`.

On PC B (`io`, `192.168.1.40`), start two terminals using the same persistent profile. In terminal 1:

```sh
target/debug/ukvm listen
```

In terminal 2:

```sh
target/debug/ukvm pair --interface enp0s20u2u1u2
```

The pairing listener makes B discoverable; the ordinary listener lets A verify B's **existing pinned certificate**. On PC A (`spectrum`, `192.168.1.21`):

```sh
target/debug/ukvm setup --interface enp10s0
```

Choose `io`. Setup should show a local pinned-ID match, authenticate it over mTLS, and continue directly to permissions and layout **without a first-contact pairing code**. Keep the existing right-edge link and permissions to check idempotency. On B, the temporary `pair` command can be cancelled after setup; no pairing exchange should have occurred. Verify both identities and A's layout/permissions afterward with `target/debug/ukvm status`, `target/debug/ukvm layout`, and `target/debug/ukvm peer permissions io`.

For a new-peer trial, use **separate temporary XDG profiles on both PCs**. In a new shell on each machine:

```sh
UKVM_SETUP_PROFILE="$(mktemp -d)"
export XDG_CONFIG_HOME="$UKVM_SETUP_PROFILE/config"
export XDG_STATE_HOME="$UKVM_SETUP_PROFILE/state"
export XDG_DATA_HOME="$UKVM_SETUP_PROFILE/data"
mkdir -p "$XDG_CONFIG_HOME/ukvm"
```

On B, write its name and start its temporary listener:

```sh
printf '[node]\nname = "io"\n' > "$XDG_CONFIG_HOME/ukvm/config.toml"
target/debug/ukvm pair --interface enp0s20u2u1u2
```

On A, write its name and run setup:

```sh
printf '[node]\nname = "spectrum"\n' > "$XDG_CONFIG_HOME/ukvm/config.toml"
target/debug/ukvm setup --interface enp10s0
```

Setup will warn about the temporary profile. Compare the code physically on both screens and confirm on both terminals. Verify both temporary trust stores afterward with `target/debug/ukvm peer list` and `target/debug/ukvm peer permissions <alias>` in their respective shells; both input permissions should remain denied unless explicitly changed. Do not reuse the name `spectrum` on a temporary identity against B's **persistent** trust store: that alias is pinned to A's different persistent ID and must conflict. Use the rebuilt `target/debug/ukvm` binary throughout, and retain the temporary directories until results are recorded because each contains a private identity.

## Limits

Setup selects pairing-mode advertisements; `ukvm discover` also lists persistent runtime/control advertisements. To live-verify an already trusted pairing hint without repeating pairing, the peer must also run its normal service/runtime or `ukvm listen` endpoint. If that endpoint is unavailable, setup may configure the pinned local record offline but will not save a discovered address. Setup can install/enable/start the existing user service only through separate explicit choices. It never grants remote-side permission or approves portal consent. See [frontend contract](FRONTEND_CONTRACT.md) for versioned JSON, layout management and applying saved changes.
