# Service, installation and daily runtime — Milestone 15

Linux systemd user service only. This service runs the existing persistent runtime with the same identity, pinned mTLS, explicit permissions and normal portal consent. No root/system input daemon or privileged input helpers are used. Input Protocol 1.3 and Pairing Protocol 2 are unchanged. M13 is physically validated. M14 partial external edges are blocked on the installed Hyprland validator; the service does not change that limitation. See [topology investigation](TOPOLOGY.md#left-capture-lifecycle-investigation).

## Installation

From the repository:

```sh
cargo build --release --workspace
./target/release/ukvm install
export PATH="$HOME/.local/bin:$PATH"
ukvm service install
```

The default prefix is `~/.local`; installed assets are `bin/ukvm`, `lib/systemd/user/ukvm.service`, and `share/doc/ukvm/SERVICE.md` plus `TOPOLOGY.md`. `ukvm service install` generates a managed unit at `${XDG_CONFIG_HOME:-~/.config}/systemd/user/ukvm.service` pointing to the absolute path of the executable invoked. Thus repository development also works: `./target/debug/ukvm service install` points to that build. Use a stable installed executable for daily use. A relocated or removed repository build cannot start its service.

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

The unit is WantedBy/PartOf the graphical-session target, ordered after graphical-session-pre.target. It never starts either target itself or runs on a boot/lingering-user default target. Its ExecCondition checks a reachable native Wayland socket without requesting consent, so stale manager environment cannot launch the input runtime outside a real graphical session. The CLI start/restart wrapper additionally requires graphical-session.target to be active. Start/restart must be invoked from a native Wayland graphical terminal. They import a fixed allowlist of graphical/session and XDG preference variables into the user manager, including the Hyprland instance signature used by the read-only emergency observer; absent values from that list are cleared to avoid importing stale session values. No arbitrary environment or secrets are imported. Root is excluded by the unit's ConditionUser.

For automatic startup at subsequent graphical logins, the desktop/session manager must correctly manage `graphical-session.target` and import current Wayland/D-Bus/desktop environment before starting it. Bare compositor launches may lack that integration. UKVM reports an inactive target rather than creating a fake session or modifying desktop configuration. Configure the desktop's documented systemd session integration; this milestone does not autoconfigure it. Multiple simultaneous graphical seats for one user are not supported; ownership is per user runtime directory.

Crash restart is bounded by RestartSec=5 and five starts per minute. Stop sends SIGINT and allows 45 seconds for the existing cooperative capture, held-input, network and portal cleanup; SIGTERM is also handled cooperatively by the runtime. Restarting/crashing invalidates portal sessions and requires normal consent again; restore tokens are not saved. No service action bypasses consent. The journal receives ordinary runtime startup/shutdown lines and errors; verbose mode adds lifecycle metadata, never individual input events. systemd being active is not proof of authenticated peer or portal readiness.

## Ownership and status

`ukvm run` takes a nonblocking kernel file lock at `$XDG_RUNTIME_DIR/ukvm/runtime.lock` before listener, discovery or portals. A second runtime explains how to inspect/stop the existing owner. The lock inode is retained, never unlinked to unlock; process exit/crash releases the kernel lock automatically. Private owned directories and regular, non-symlink lock files are required. A stale PID/snapshot never grants ownership or prevents recovery.

The owner atomically writes a private, bounded `status.toml` snapshot once per second, containing public node identity, focus, active peer/coordinate, independent topology readiness and portal/EI lifecycle state. It contains no key material, events or commands. Status never controls the daemon. Snapshots older than five seconds or unreadable/malformed are marked unknown; snapshots without a held lock are ignored. This is advisory sampling, not an IPC/control API or an instantaneous health guarantee. Shutdown removes snapshot/pending files while retaining the lock inode.

`ukvm status` remains useful offline: it reads existing public identity, config and trust without creating an identity; shows configured coordinate topology and user-service state; and distinguishes unknown live availability from authenticated readiness. `ukvm peers` lists every trusted alias, full pinned ID, connect/debug/control permissions and availability where the runtime has a configured outgoing observation. Non-layout peers have unknown outgoing availability. These views grant no trust or input permission. The runtime loads preferences at startup; disk changes require restart. Live backend details come from the running process, not the status terminal's environment. Offline platform probing is capability discovery only and does not request portal consent.

## Acceptance

Automated tests cover unit rendering/escaping and refusal to overwrite custom units, distro staging, exclusive runtime ownership, stale/malformed/symlink state, service command selection, status/peers offline output, and clean SIGINT/SIGTERM shutdown with reacquisition. All required workspace checks passed locally: fmt, check, test (170 passed, five opt-in tests ignored), Clippy with warnings denied, and build. A real temporary installation and systemd-analyze --user verify of its unit also passed; no real user service was installed/enabled/started during validation. Physical systemd/portal acceptance is pending; no unattended login, service switching or crash recovery is claimed as physically tested yet.

On spectrum and io, use a previously working full-edge topology (do not try to bypass Hyprland's partial-edge blocker): install, explicitly enable/start, approve consent, inspect status/peers and journal. Attempt `ukvm run` while the service owns it: it must refuse without opening portals. Repeat normal switching/return and emergency inward rearm, then stop/restart a receiver and verify recovery. Stop a service during held remote input and check local restoration/held-state cleanup. Send SIGTERM and verify cooperative stop; deliberately kill only UKVM in a controlled test and check bounded crash restart plus fresh consent. Log out/in and verify service follows the graphical session, with correct environment; disable must prevent the next login's automatic start. Finally stop/disable/uninstall and confirm foreground development still works and identities/trust persist. Record only what was actually tested.
