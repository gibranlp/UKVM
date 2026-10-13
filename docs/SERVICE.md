# Service, installation and daily runtime — Milestone 15

Linux systemd user service only. This service runs the existing persistent runtime with the same identity, pinned mTLS, explicit permissions and normal portal consent. No root/system input daemon or privileged input helpers are used. Input Protocol 1.3 and Pairing Protocol 2 are unchanged. M13 is physically validated. M14 partial external edges are blocked on the installed Hyprland validator; the service does not change that limitation. See [topology investigation](TOPOLOGY.md#left-capture-lifecycle-investigation).

## Installation

For distro packages and GitHub download/release instructions, see
[Linux packaging](PACKAGING.md). Packages own the `/usr` vendor assets and leave
service activation and existing user state to the user.

From the repository:

```sh
cargo build --release --workspace
./target/release/ukvm install
export PATH="$HOME/.local/bin:$PATH"
ukvm service install
```

The default prefix is `~/.local`; installed assets are `bin/ukvm`, `lib/systemd/user/ukvm.service`, and `share/doc/ukvm/SERVICE.md`, `TOPOLOGY.md`, `FRONTEND_CONTRACT.md` and `cli-schema-v1.json`. `ukvm service install` generates a managed unit at `${XDG_CONFIG_HOME:-~/.config}/systemd/user/ukvm.service` pointing to the absolute path of the executable invoked. Thus repository development also works: `./target/debug/ukvm service install` points to that build. Use a stable installed executable for daily use. A relocated or removed repository build cannot start its service.

The user command writes/reloads the unit but does not enable or start it; reinstall preserves existing enabled/running state. Unmanaged units and symlink unit destinations are not overwritten. Exec paths are quoted with escaped systemd specifiers; the colon executable prefix disables environment-variable expansion. `ukvm service install --verbose` selects verbose runtime lifecycle logs; reinstall without the flag restores concise logs for the next explicit restart. Existing identity, configuration and trust are never copied or replaced by installation.

Packagers can stage without root or operating the host's service manager:

```sh
./target/release/ukvm install --prefix /usr --destdir /absolute/staging
```

This produces `/absolute/staging/usr/bin/ukvm`, `/absolute/staging/usr/lib/systemd/user/ukvm.service` with final ExecStart `/usr/bin/ukvm`, and documentation. No distro-specific tooling or SpectrumOS code is included. The user unit installed with `service install` takes precedence over a vendor unit. `service uninstall` removes only the managed config unit; package ownership controls vendor assets and binaries.

## Explicit service actions

```sh
ukvm service enable
ukvm service start
ukvm service status
ukvm status
ukvm peers
journalctl --user -u ukvm
ukvm service restart
ukvm service stop
ukvm service disable
ukvm service uninstall
```

Enable schedules future graphical-session startup; it does not start now. Disable removes future startup; it does not stop a running service. Uninstall explicitly disables/stops and removes the managed user unit, retaining the binary and all private identity/config/trust data. Packaged vendor units may remain.

The unit is WantedBy/PartOf `graphical-session.target`, ordered after `graphical-session-pre.target`. It never starts either target itself or runs on a boot/lingering-user default target. Root is excluded by `ConditionUser`. There is no `ConditionEnvironment=WAYLAND_DISPLAY` or `ExecCondition`: early session/backend unavailability exits with a retryable failure instead of permanently skipping that login. systemd retries after five seconds; twenty starts per minute allow sustained readiness retries without a busy loop. Explicit stop and graphical-session teardown cancel restart.

The internal `service run` launcher queries the current user manager environment on every attempt. It requires one active logind Wayland session and a starting/active graphical-session target. It validates socket ownership, connectivity, compositor peer credentials and process age against the current login, rejecting stale sockets even when the user manager survives logout. Hyprland can recover missing/stale manager environment from its native runtime lock and control socket, with matching compositor PID and exactly one fresh instance. Other Wayland desktops use their current manager environment and retry until it becomes available. Ambiguous simultaneous graphical logins are refused.

The unit supplies `XDG_RUNTIME_DIR=%t` and the canonical per-user D-Bus address. The launcher publishes only validated Wayland/desktop variables to systemd and D-Bus activation before checking portal availability. It does not replace arbitrary manager variables or import secrets. Readiness requires the portal frontend and at least one input portal interface; this check requests no consent. Only then does the launcher replace itself with the existing runtime, preserving PID, signal delivery and single-instance ownership. Normal portal authorization and consent still happen in the runtime; no restore tokens are saved.

`ukvm service check-session` performs the same read-only current-session/socket validation without publishing environment, activating portals, creating identity or taking runtime ownership. Manual start/restart still require a native Wayland graphical terminal and import a fixed allowlist of session/XDG preference variables automatically; users need no manual `systemctl import-environment` command.

The desktop must manage `graphical-session.target` with its documented systemd session integration and register a Wayland login with logind. Hyprland with UWSM is supported; bare compositor launches without that lifecycle are reported rather than creating a fake session or modifying desktop configuration. No shell-profile hooks or SpectrumOS configuration are installed. Other desktops still need to publish their session environment eventually. Multiple simultaneous graphical seats for one user are not supported; ownership is per user runtime directory.

Stop sends SIGINT and allows 45 seconds for cooperative capture, held-input, network and portal cleanup; SIGTERM is also handled cooperatively. Restarting/crashing closes active portal sessions. Supported remembered authorization is restored through the official portal API; otherwise normal consent is required. The journal receives startup/shutdown lines and readiness failures; verbose mode adds lifecycle metadata, never individual input events. systemd being active is not proof of authenticated peer or portal readiness.

## Ownership and status

`ukvm run` takes a nonblocking kernel file lock at `$XDG_RUNTIME_DIR/ukvm/runtime.lock` before listener, discovery or portals. A second runtime explains how to inspect/stop the existing owner. The lock inode is retained, never unlinked to unlock; process exit/crash releases the kernel lock automatically. Private owned directories and regular, non-symlink lock files are required. A stale PID/snapshot never grants ownership or prevents recovery.

The owner atomically writes a private, bounded `status.toml` snapshot once per second, containing public node identity, focus, active peer/coordinate, independent topology readiness and portal/EI lifecycle state. It contains no key material, events or commands. Status never controls the daemon. Snapshots older than five seconds or unreadable/malformed are marked unknown; snapshots without a held lock are ignored. This is advisory sampling, not an IPC/control API or an instantaneous health guarantee. Shutdown removes snapshot/pending files while retaining the lock inode.

`ukvm status` remains useful offline: it reads existing public identity, config and trust without creating an identity; shows configured coordinate topology and user-service state; and distinguishes unknown live availability from authenticated readiness. `ukvm peers` lists every trusted alias, full pinned ID, connect/debug/control permissions and availability where the runtime has a configured outgoing observation. Non-layout peers have unknown outgoing availability. These views grant no trust or input permission. The runtime loads preferences at startup; use `ukvm apply` to validate saved changes and restart the managed service. It refuses a foreground/unknown runtime owner, never launches a second foreground process, and preserves the M15 restart/consent lifecycle. Status/peers/layout/doctor/discover and explicit service actions have versioned JSON; see [frontend contract](FRONTEND_CONTRACT.md). Live backend details come from the running process, not the status terminal's environment. Offline platform probing is capability discovery only and does not request portal consent.

## Acceptance

Automated tests cover unit rendering/escaping and refusal to overwrite custom units, distro staging, exclusive runtime ownership, stale/malformed/symlink state, service command selection, status/peers offline output, and clean SIGINT/SIGTERM shutdown with reacquisition. Login regressions exercise the real launcher against private D-Bus/session/portal doubles: initially absent Wayland environment, late Hyprland/socket/portal readiness, late generic desktop environment, stale live compositor and dead socket rejection, single-instance ownership, inactive graphical-session refusal, and SIGTERM cleanup/restart. The private bus disables installed service activation. Full workspace fmt, check, test (173 passed, five opt-in tests ignored), Clippy with warnings denied, build and staged-unit verification passed.

User physical testing from `9207ab8e8781ca69389734913b9ef41dfbd44885` passed normal service operation, switching, single-instance protection, service restart recovery and logout cleanup. Login autostart failed because the old environment condition was evaluated too early. The follow-up at `79d088ef0f996e159b8f6b9e9fb80bde790e5f00` physically starts UKVM automatically and keeps it active after logout/login. A separate pinned development portal initialization failure is tracked in [trial lifecycle validation](WAYLAND_INJECTION_TRIAL.md#m15-logoutlogin-lifecycle-regression); the persistent trial fix was physically validated through M15, as reported by the user. The read-only session check passes on the development PC's actual Hyprland/UWSM login. No real UKVM user service was installed/enabled/started during automated validation.

### Login regression retest

On io, update and regenerate the managed unit, then enable it:

```sh
cd ~/UKVM
git pull --ff-only origin main
cargo build --release --workspace
./target/release/ukvm install
~/.local/bin/ukvm service install --verbose
~/.local/bin/ukvm service enable
```

Log out of Hyprland completely, then log back in normally. Do not manually import environment or run `service start/restart`. Approve normal portal consent if requested. Check:

```sh
~/.local/bin/ukvm service status
~/.local/bin/ukvm status
journalctl --user -u ukvm -b --no-pager -n 50
```

Expect an enabled, active/running service and a current runtime snapshot, with no unmet Wayland condition. Brief readiness retries may precede runtime startup. Verify normal switching on the previously working full-edge topology. Log out again to verify cleanup and repeat login to check that lingering manager environment does not select an old socket. Disable must prevent the next login's automatic start. M14's partial-edge blocker remains unchanged. M16 adds management UX without changing the service startup fix.

## Fresh receiver authorization after Apply

`ukvm apply` already validates saved state and performs one controlled managed-user-service restart; preferences are loaded at runtime startup and there is no general reload API. Startup now prepares the shared RemoteDesktop/EI receiver when at least one pinned peer has both `connect` and `control-this-node`. It does not wait for a peer to reconnect or attempt edge entry. Fresh installations request normal explicit portal consent; existing remembered authorization follows the unchanged persistence path. Merely pairing a peer, clipboard permission, and file permission never trigger receiver consent.

Preparation runs concurrently with accepting connections and other runtime workers, uses the same serialized initialization as authenticated incoming sessions, and never enters remote focus or injects input. Cancellation/permission revocation interrupts pending preparation. Denying control continues to terminate active input through the live policy monitor; Apply closes the previous receiver with cooperative service shutdown. If no control-permitted peer remains, the replacement runtime does not request receiver authorization. Consent rejection/backend failure is visible in receiver status; startup does not repeatedly prompt. An explicitly authorized incoming request or a later explicit Apply/restart can retry.

The former receiver was lazy: its initial `consent not requested` state changed only after an authenticated RemoteSessionStart. Apply and manual restart used the same restart operation, so a successful Apply alone did not establish receiver readiness; observed consent depended on incoming reconnect/request timing. Automated regressions cover deny→allow→Apply's managed lifecycle, proactive authorization without a network request, allow→deny→Apply, the connection-permission gate, revocation during pending consent and shutdown cancellation. Fresh-install portal-dialog and physical control acceptance remain required.

## Interactive Remote Sessions v1

The owned runtime additionally serves Display Protocol 1 on its existing pinned
TLS listener and a private same-user native-window input bridge. Display and GUI
failures stay independent of physical KVM, clipboard and file workers. Source
window entry shares the focus gate; release restores physical capture eligibility.
The socket is private and removed on cooperative shutdown; startup only removes
a stale owned socket after taking the existing permanent runtime lock. No service
start/enable is implied by opening a window. Both peers need Input Protocol 1.4.
Installation also includes `REMOTE_SESSIONS.md`. See [test commands and dependencies](REMOTE_SESSIONS.md).
