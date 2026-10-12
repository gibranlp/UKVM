# Persistent desktop input authorization

UKVM peer trust and desktop portal approval are independent. Pairing never requests portal consent or grants `control-this-node`. A receiver still authenticates each peer, checks its pinned identity and local control permission, and checks permission throughout control. A remembered portal grant only lets UKVM obtain the same local desktop input session without asking again; it never authorizes a peer or opens an input API to the network.

## Official protocol and capability limits

RemoteDesktop v2 supports `persist_mode = 2` and `restore_token` on SelectDevices. UKVM requests keyboard/pointer as before and asks the portal to remember consent. The portal user must explicitly approve both control and remembering it. On Start, UKVM stores the returned replacement restore token before connecting to EIS. Subsequent sessions present that token, still request the same devices, and still validate the portal's returned device grants. Tokens are single-use: a token is removed from durable state before an attempt, and a successful response replaces it atomically. Cancellation, rejection, timeout or process loss cannot cause UKVM to reuse a consumed token. A missing, expired or revoked token follows normal portal consent; an outright failed request returns a safe error and the next attempt uses normal consent.

InputCapture v2 supports the corresponding Start options and replacement token. UKVM uses them consistently. InputCapture v1 retains its normal permissioned CreateSession behavior and reports persistent authorization as unavailable. API support is determined from the interface version; actual permission to persist is determined only by a returned token. A v2 portal can decline persistence or omit a token: this is an active session grant, not a promise of unattended restoration.

Official specifications: [RemoteDesktop](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html), [InputCapture](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.InputCapture.html).

SpectrumOS's pinned XDPH PR #426 revision `0204ffbbcb3cef72af76804cdf59f114f2e2f6d9` already implements persistent RemoteDesktop restore data and a consent checkbox labelled “Allow … to skip this prompt in the future,” unchecked by default. UKVM does not select that checkbox. The backend's own grant storage and the portal frontend's token storage remain authoritative. No XDPH revision, trial script, compositor barrier behavior or production session helper is changed. Its InputCapture interface is v1, so controller-side persistent capture approval is unavailable there.

## Private state, ownership and reset

Credentials live in `$XDG_STATE_HOME/ukvm/portal-authorization/`, defaulting to `~/.local/state/ukvm/portal-authorization/`. UKVM requires owned 0700 directories, owned regular 0600 files, and refuses symlink state/lock files. Records are bounded and malformed records require normal consent. Replacements use private temporary files, atomic rename and file/directory synchronization. A permanent per-interface lock inode serializes single-use consumption and remains owned for the portal session's lifetime. Read-only status does not create credentials or sessions. Secrets have no Debug representation and are never included in public status, JSON, normal/verbose logs, or widget responses; credential-bearing request failures use fixed safe messages.

State is scoped to the interface, the running portal frontend's desktop identity and portal API version. UKVM resolves the same session-bus portal owner for runtime and read-only CLI status, verifies its UID, reads only its desktop label, and rechecks ownership against a concurrent restart. Caller desktop aliases or missing terminal/SSH environment do not select the context. Owner/PID, Wayland socket, compositor signature, login IDs and package patch versions are never persisted as identity. Read-only status reports `context_verified: false` when the frontend cannot be observed; it still reports remembered storage without declaring a backend change or deleting credentials. Runtime restoration requires an observed portal context. Desktop/API changes discard the application-side restore attempt and require normal consent. Backend vendor and restore-data version are validated by the official portal/backend protocol; UKVM does not pretend that a frontend interface version identifies a particular backend build, pin transient D-Bus owners, or invalidate grants merely because a package patch version changes. Incompatible backend changes may trigger normal consent even when the desktop/API labels remain unchanged.

```sh
ukvm authorization status
ukvm authorization status --json
ukvm authorization reset --yes --json
```

Reset requires terminal confirmation or explicit `--yes` after frontend confirmation. It verifies runtime ownership, cooperatively stops the managed UKVM user service, requires the runtime to have released ownership, then forgets the RemoteDesktop, InputCapture and ScreenCast credentials. A foreground/unknown runtime or another diagnostic portal session blocks reset. UKVM stays stopped; the service's enabled setting is unchanged. Start explicitly to request normal consent again. Reset does not edit peer pins, control permissions, identity or topology, and does not auto-start a new permission prompt. Partial storage failures return nonzero, explaining if the service was already stopped. This is forgetting UKVM's credentials and closing its active grant, not a desktop-wide revocation tool for other applications or copies of credentials; revoke desktop-wide permissions through the desktop's portal controls when available.

## Status and frontend contract

Schema version 1 gains `desktop_authorization` on status and doctor, with `remote_desktop` and `input_capture` entries. Each has `state` (`persistent`, `active`, `needs_approval`, `unsupported`, `unavailable`), `active`, `remembered`, nullable `persistence_api_supported`, safe `reason`, and optional safe `error`. `persistent` means a token is remembered, not proof it will restore or that physical input worked. `active` requires the current authorization lock plus a successfully granted portal response. A crash-released lock cannot claim a stale grant is active. Doctor remains read-only and its legacy `authorization: not_tested` / `physical_input: not_tested` fields retain their meaning. Frontends must continue checking peer permissions and backend/service health independently.

SpectrumOS shows Remote control authorization and a confirmed **Details → Forget approval** action. It invokes the documented CLI, never edits credentials or reads tokens. InputCapture persistence availability is shown under Details.

## Same-session acceptance failure and correction

r18-2 physically granted control but immediately reported `backend_changed`. It bound records to the UKVM service's `XDG_CURRENT_DESKTOP` and compared them with the querying process's environment. The service normalizes Hyprland while terminal/SSH/widget environments can differ or omit the label. This diagnostic masked both token presence and live authorization; it did not itself delete the token, but differing runtime labels could discard it on a later attempt. There were no volatile session/PID/socket/package values in that original record. The fix obtains desktop context from the actual portal frontend for both paths, retaining desktop/API incompatibility checks and the backend's vendor/version/grant validation. Existing matching private tokens are retained. A legacy record from a genuinely different portal context needs normal approval again.

The pinned XDPH backend returns backend-only `restore_data` (vendor `hyprland`, restore-data version 3), not the app-facing restore token. The official portal frontend wraps it in an app-facing token. In frontend 1.22.1 the lookup consumes the saved permission entry and a successful Start reissues the token; its string may be unchanged. XDPH also intentionally reuses its internal remembered grant. UKVM must accept the token returned by successful Start regardless of string equality, without replaying a consumed credential after failure. Mode 2/returned-token handling, atomic private writes and consume-before-use remain unchanged and covered by D-Bus tests.

## Physical acceptance (correction not yet physically retested)

The user reports production packages, pairing, RemoteDesktop consent, physical control and post-reboot recovery passed on a new SpectrumOS laptop before this persistence change. That does not establish unattended restoration. Keep the existing trusted relationship and full external-edge topology intact.

1. Install the updated UKVM package/CLI on both nodes through the managed package path. The existing pinned portal package remains unchanged. Open the widget and inspect health, permissions and authorization state.
2. On the controlled node run `ukvm service restart`. Initiate control from the already trusted, permitted peer. Explicitly approve the portal and select its remember/skip-future-prompts checkbox. Confirm keyboard, pointer, buttons, scroll and ReturnControl. `ukvm authorization status --json` must report RemoteDesktop `persistent` with `remembered: true`; a missing token is a failure of persistence acceptance even if current control works.
3. Run `ukvm service restart` again on the controlled node. Initiate control without interacting locally with a portal dialog. Confirm the same input functions, ReturnControl and emergency release. Re-query status; credentials must remain remembered after rotation. Never inspect or print the credential files.
4. Log out completely, log back into the supported graphical session, and initiate control again without approving another portal dialog. Repeat after reboot. A supported logged-in graphical user session is still required; this change does not control the display-manager greeter or configure automatic login. To test a machine without attached input after reboot, use an already explicitly configured graphical auto-login test machine.
5. Restart the backend through its normal user service: `systemctl --user restart xdg-desktop-portal-hyprland.service`. Reconnect and confirm restoration, cleanup, input and ReturnControl. The managed package orders the frontend; no UKVM portal restart workaround is added.
6. Test reset deliberately: `ukvm authorization reset --yes --json`. Verify UKVM is stopped, no remembered grants remain, and peer permission/topology views are unchanged. Run `ukvm service start`, initiate control, and verify normal portal approval is required again. Reject once and confirm no input occurs; approve/remember only if desired. On a disposable profile additionally test portal-side revocation/expiry and incompatible backend changes; they must prompt normally or report safe failure, never inject without consent.

Automated coverage includes private permissions and symlink refusal, bounded/corrupt/missing records, desktop/API changes, token creation/consumption/rotation, invalid credentials, rejected restoration, crash/restart semantics, confirmed reset/ownership refusal, actual SelectDevices/Start D-Bus options, returned device denial, JSON/log redaction, independent peer policy and widget confirmation. Actual unattended logout/login/reboot behavior awaits this procedure.

## r18-3 restoration failure: reissued token value

After r18-3 corrected the context mismatch, the user physically observed a valid active remembered token, followed by another RemoteDesktop consent dialog after service restart. The installed frontend is `xdg-desktop-portal 1.22.1-2`, with pinned XDPH `1.4.1.r30.g0204ffb-1`. The faulty UKVM check rejected successful Start when its returned token equalled the submitted string. This discarded a restored grant and consumed the local credential, so a later attempt requested fresh consent. The D-Bus regression reproduces that precise rejection using the real UKVM SelectDevices/Start path.

[Frontend 1.22.1 persistence implementation](https://github.com/flatpak/xdg-desktop-portal/blob/1.22.1/src/xdp-session-persistence.c) consumes the permission-store entry during SelectDevices, retains the opaque token identifier, and saves/returns it again after Start; single-use authorization does not guarantee unique string bytes. UKVM now persists the returned credential from successful Start, whether changed or reissued. Atomic writes, private ownership, exact string preservation, consume-before-attempt safety, granted-device checks and peer authentication/permissions remain required. Failed or cancelled responses never renew credentials. No XDPH, frontend, compositor, portal lifecycle or consent patch is needed.

Verbose diagnostics report mode 2, whether a restore token was supplied, Start success, pointer/keyboard grants, whether a replacement was received, whether its value was reissued and successful durable storage. They never print token bytes, hashes or raw response dictionaries. The public response does not distinguish restored consent from normal consent; logs explicitly say `restoration_confirmation=not_exposed_by_portal` and do not claim that a dialog was skipped. No UKVM restoration error is silently turned into success.

For a physical retest, install the new package, then `ukvm service install --verbose` and `ukvm service restart`. If the failed r18-3 attempt already consumed the old credential, explicitly approve/remember once to establish a fresh grant. Query `ukvm authorization status --json`, restart again, initiate control and require no new RemoteDesktop dialog. Inspect `journalctl --user -b -u ukvm.service --no-pager`; a reissued value must reach durable storage and ConnectToEIS, without the old rejection or an extra fresh authorization request. Then test logout/login and reboot. Do not reset authorization or print credential files. This correction is not physically accepted until that test passes.

## ScreenCast remembered monitor (M21)

Interactive remote viewing requests official ScreenCast v4+ `persist_mode=2` and
`restore_token` on SelectSources, with one monitor and an embedded cursor. Start
must succeed and return exactly the requested monitor before its replacement
token is stored. OpenPipeWireRemote then uses only the portal-restricted FD.
ScreenCast has a separate `screen-cast` credential/lock in the same private store;
it never consumes, renews or grants RemoteDesktop/InputCapture permission.
ScreenCast interfaces before v4 omit persistence options and use normal consent.
A v4+ backend may omit a token, ignore an expired grant or prompt again: API
support/token presence is not evidence of unattended restoration.

The first approved source may be remembered until explicitly revoked by the
portal. UKVM consumes before attempting, durably replaces tokens after successful
Start, accepts a reissued value, and uses fixed errors for credential-bearing
requests. Context/version changes, rejection, cancellation and crashes retain the
existing fail-closed rules. A ScreenCast lease lasts through capture cleanup.
`authorization status` adds the redacted `screen_cast` entry; confirmed reset now
locks/forgets all three kinds and cannot forget an active sharing lease.
[Official ScreenCast persistence specification](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html).

Private-bus regressions exercise real ashpd SelectSources/Start requests for v3,
v4, rotation/reissued values, denied restoration and missing tokens. Private-state
tests cover independent credentials, redacted status and reset refusing an active
ScreenCast lease. The user confirmed M21 viewing/control and the corrected redraw
behavior; unattended ScreenCast restoration after close/restart/login/reboot is
still pending. See [the focused retest](REMOTE_SESSIONS.md#remembered-sharing-and-quality-retest).
