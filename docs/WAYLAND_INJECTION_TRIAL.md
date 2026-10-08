# Milestone 12 Wayland injection trial

This is an **experimental, user-session-only** trial for io. Local pointer injection and two-node pointer, button, scroll, and keyboard input have been physically observed on io. Three consecutive natural edge returns, emergency release, and normal local input after abrupt sender loss with Shift and a mouse button held have also been observed. Keep the packaged portal available for rollback. Do not use the older generic portal revision: its inspected RemoteDesktop `Start` did not mediate user consent.

## Current evidence and dependency

On spectrum, Hyprland 0.56.2-3, xdg-desktop-portal 1.22.1-2, xdg-desktop-portal-hyprland 1.4.1-2, xdg-desktop-portal-gtk 1.15.3-1, and libei 1.6.0-1 were installed. `ukvm doctor` found InputCapture v1 with pointer and keyboard, but no RemoteDesktop. The packaged `hyprland.portal` does not advertise RemoteDesktop. io is expected to have the same SpectrumOS package set, but its own runtime versions and portal state must be checked before the trial.

The candidate is Hyprland's [draft xdg-desktop-portal-hyprland PR #426](https://github.com/hyprwm/xdg-desktop-portal-hyprland/pull/426), pinned at `0204ffbbcb3cef72af76804cdf59f114f2e2f6d9`. Its source was inspected and built on spectrum and io. The draft adds RemoteDesktop v2 and `ConnectToEIS`, a local consent picker, pointer/button/scroll and keyboard EI devices, and retains InputCapture. The backend uses compositor virtual pointer/keyboard protocols within the portal grant; UKVM itself only connects to the portal-provided EIS socket. The draft remains unmerged. Local pointer motion and cross-PC pointer, button, scroll, and keyboard input were observed with UKVM on io.

## Physical local pointer result (2026-09-28)

After activating the pinned draft on io, `ukvm doctor` in its Hyprland session reported a reachable Wayland socket, InputCapture v1 with keyboard/pointer, and RemoteDesktop v2 with keyboard/pointer and `ConnectToEIS`. The user ran `ukvm platform wayland inject-test`, approved the local portal prompt, and observed io's cursor move right and then left. The command was launched through SSH while io's graphical session was running; physical observation and portal consent occurred on io. This proves permissioned local relative pointer motion through UKVM's RemoteDesktop/EI sender. The later two-node test below covers remote pointer, button and keyboard input. It does not prove scroll events, held-input cleanup after disconnect, or seamless edge return.

## Physical two-node result (2026-09-28)

With `ukvm listen --remote --bind 0.0.0.0:24800` running on io and `ukvm control right` on spectrum, the two nodes authenticated with pinned mutual TLS. io authorized the remote session through its portal before spectrum created InputCapture. Spectrum's portal accepted right-edge barrier 1 on zone 1 from `(0, 0)` to `(0, 2159)`, then capture activated. The sender counted 1087 pointer, 8 button, 0 scroll, and 116 keyboard events, with zero dropped pointer events, 32992 input-frame bytes, and about 1.39 ms heartbeat RTT. The user physically observed io's pointer move, clicks take effect, and keyboard input take effect. After 30 seconds, spectrum's portal `Release` succeeded, its session closed, and local control returned normally. No key contents were recorded.

A later short capture sent 173 pointer, 2 button, 0 scroll, and 11 keyboard events with zero dropped motion. The user confirmed Ctrl+Alt+Shift+Escape ended capture before its 30-second active limit, portal `Release` succeeded, and local mouse and keyboard control returned normally. Scroll delivery remains unproven because the user did not scroll during this test. A preceding retry reached peer authentication but stalled before remote-session acceptance and showed no portal prompt; restarting io's listener allowed the next attempt to complete. The cause of that stall was not established.

## Build on io, without installation

On io, with its Hyprland graphical session running and the Milestone 12 UKVM
revision pulled and built, fetch and build the draft. Fetching and building may
be done over SSH. The later physical pointer test needs someone watching io's
monitor and approving its portal prompt there.

The draft's CMake build uses the Ninja generator and requires the
`hyprwayland-scanner` pkg-config module. On a fresh Arch test node, install all
three build tools before configuring it. Review the pacman transaction before
confirming the full system upgrade:

```sh
sudo pacman -Syu --needed cmake ninja hyprwayland-scanner
```

The UKVM Arch setup script also installs these tools on future test nodes.

```sh
cd ~/UKVM
target/debug/ukvm doctor
pacman -Q hyprland xdg-desktop-portal xdg-desktop-portal-hyprland xdg-desktop-portal-gtk libei
git clone --recurse-submodules https://github.com/hyprwm/xdg-desktop-portal-hyprland.git /tmp/ukvm-xdph-pr426
git -C /tmp/ukvm-xdph-pr426 fetch origin 0204ffbbcb3cef72af76804cdf59f114f2e2f6d9
git -C /tmp/ukvm-xdph-pr426 checkout --detach 0204ffbbcb3cef72af76804cdf59f114f2e2f6d9
git -C /tmp/ukvm-xdph-pr426 submodule update --init --recursive
cmake -S /tmp/ukvm-xdph-pr426 -B /tmp/ukvm-xdph-pr426-build -G Ninja -DCMAKE_BUILD_TYPE=Release
cmake --build /tmp/ukvm-xdph-pr426-build -j 4
```

The source build is not installed system-wide. Inspect the pinned revision and build output before activation. The script below refuses any other source revision or missing backend/picker executables.

## Activate and verify

```sh
cd ~/UKVM
cargo build --workspace
scripts/wayland-portal-trial start /tmp/ukvm-xdph-pr426 /tmp/ukvm-xdph-pr426-build
scripts/wayland-portal-trial status
target/debug/ukvm doctor
```

The script copies the installed portal descriptors and the effective `hyprland-portals.conf` into `${XDG_DATA_HOME:-$HOME/.local/share}/ukvm/portal-trial/portals`, replacing only that **copy** of `hyprland.portal` with the pinned draft. It copies the development `ukvm-portal-trial-session` helper, pinned backend binary and consent picker into the private persistent trial directory and adds two named systemd user drop-ins under `${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user`. After successful installation, the original source/build and Cargo helper artifacts are no longer required to run or stop the trial. The frontend gets `XDG_DESKTOP_PORTAL_DIR` pointing to the copied descriptors/configuration; the backend runs the built draft and puts its consent picker on `PATH`. Both use the helper to resolve the current Hyprland session before replacing themselves with the actual portal executable. The helper is not installed by `ukvm install`; this remains development tooling. An alternate Cargo target directory can be selected with `UKVM_TRIAL_HELPER=/absolute/path/to/ukvm-portal-trial-session`.

Before stopping anything, the script validates the current Hyprland session and publishes only its fixed graphical environment allowlist to systemd and D-Bus activation. It then stops both old portal processes, resets only units actually in the failed state, and starts the frontend through its backend dependency. An inactive/static D-Bus unit may be unloaded after stop; that is normal and does not require `reset-failed`. The frontend is ordered after the backend, including its `ExecStartPost` capability check. Both units are bound to the graphical-session target; the frontend also follows backend stop/restart. Backend start/retry pulls the frontend back in. The packaged backend's early `ConditionEnvironment` is cleared only for the trial. Unavailable sessions/interfaces fail and retry with systemd's five-second restart delay; no readiness sleeps or restart logic are added to UKVM.

The helper reuses the read-only current-session resolver, requiring one active logind Wayland login and a current native Hyprland instance. It validates owned live sockets and compositor age, recovers the current display and instance signature from Hyprland runtime state, uses `%t` and its per-user D-Bus, and removes inherited `WAYLAND_SOCKET`. Old manager values are not authoritative. Readiness checks use the unique D-Bus owner's process environment, including the instance signature, so a reused display name cannot make an old portal process pass. InputCapture v1+ and RemoteDesktop v2+ must both advertise keyboard/pointer. These property queries request no grants and inject no input.

Setup records ownership separately from readiness (`preparing`, `active`, `restoring`, `broken`). Success requires both services active and both read-only capability/session checks passing. A failed setup exits nonzero and attempts to restore both packaged services in backend/frontend order, checking each service independently. Failure clearing cannot abort restoration merely because a stopped static unit was unloaded. Genuine manager/restart errors remain visible; an incomplete restoration retains the recovery marker for `stop` to retry. Exact saved drop-in copies protect modified/unrelated overrides from deletion. A failed setup never prints trial success.

`status` distinguishes an active trial, stopped trial, broken/partial setup and no installed trial. With no marker it still reports whether the ordinary portal stack is inactive. It reports unit dependencies, PID, restart count and result, then checks current portal owners and capabilities without activating either service. A legacy trial without the helper is reported explicitly: stop it, build UKVM and start the trial again. The development installation survives runtime-directory deletion and user-manager recreation; native portal D-Bus activation loads its persistent drop-ins in the new manager. No display name, compositor signature or session environment is stored in the installation. Session bindings stop the portals at graphical logout even when the manager survives. Missing persistent assets/drop-ins are reported as broken. No packaged binary, system portal configuration, user trust record or private key is edited. The unchanged copied `hyprland-portals.conf` continues routing InputCapture to Hyprland. Run `stop` to remove the persistent trial integration; logout or reboot does not uninstall it. Legacy runtime trials must be stopped before installing this version; unrelated or modified runtime overrides are refused.

Before continuing, `doctor` must still show InputCapture with pointer capture and must now show RemoteDesktop v2, pointer, keyboard, and `ConnectToEIS`. If InputCapture disappears or the services fail, roll back immediately.

## M15 logout/login lifecycle regression

UKVM commit `79d088ef0f996e159b8f6b9e9fb80bde790e5f00` physically starts automatically and remains active after Hyprland logout/login. The separate pinned portal trial could omit RemoteDesktop or advertise zero input devices until both portal services were restarted manually. Trial integration now binds both processes to the graphical session, rebuilds their environment at each launch and orders verified backend initialization before frontend interface caching. The pinned upstream revision and UKVM runtime startup fix are unchanged.

Automated coverage uses private buses with installed service activation disabled, checks missing/zero/ready backend interfaces, rejects stale process environments despite reused display names, and exercises late Hyprland readiness without inherited Wayland environment. The initial trial lifecycle implementation at `b3501dae1f430f19c3347696afea3022e632ece8` failed physical startup: unconditional `reset-failed` on unloaded static units aborted setup, then the same command aborted rollback after removing overrides, leaving portals stopped. The transaction recovery follow-up addresses that separate failure; UKVM runtime code remains unchanged.

Script tests now model unloaded/static D-Bus units, real reset errors, skipped starts, partial activation, capability-check failure, restoration failure/retry, inactive/broken status and modified/unrelated drop-ins. Generated units pass systemd-analyze verification. Disposable static D-Bus services on the actual systemd user manager reproduced the unloaded-unit error and verified frontend/backend ordering, stop propagation, startup after unload and backend restart propagation. These used unique test service/bus names and were removed afterward. Validation passed: workspace fmt, check, test (183 passed, five opt-in tests ignored), Clippy with warnings denied and build, plus Bash syntax, ShellCheck and isolated systemd unit verification. No real portal services were operated by these tests. Physical testing of `26bc33c0674cc1da91921aeb5d05ef99edef91fc` confirmed setup and both portal capabilities before logout, but full logout deleted the runtime-only definition. The persistent installation fixes that separate lifetime error while retaining current-session resolution, ordering and transaction recovery. Regression tests delete/recreate the runtime directory, model both surviving and recreated managers, remove the original build/helper, verify persistent assets and activation/status, then restore stock portals using the copied helper. They also cover missing persistent assets and legacy orphan refusal. Full graphical logout/login validation of this persistence change remains pending.

From the reported post-login state on io (no trial marker and normal packaged services), with the existing valid pinned source/build artifacts present, start directly; do not run `stop` against a missing marker and do not rebuild XDPH:

```sh
cd ~/UKVM
git pull --ff-only origin main
cargo build --workspace
ukvm service stop
scripts/wayland-portal-trial start /tmp/ukvm-xdph-pr426 /tmp/ukvm-xdph-pr426-build
scripts/wayland-portal-trial status
ukvm doctor
ukvm service enable
hyprctl dispatch exit
```

Log back into Hyprland normally, without importing environment, restarting portals or manually starting UKVM. Run:

```sh
cd ~/UKVM
scripts/wayland-portal-trial status
ukvm doctor
ukvm service status
ukvm status
```

Expect keyboard/pointer on both interfaces, RemoteDesktop v2/ConnectToEIS and an automatically running UKVM service. Approve normal consent and physically repeat spectrum ⇄ io control. Repeat logout/login to test a surviving user manager. M16 remains unstarted.

## Local injection proof, then remote proof

On io, run `target/debug/ukvm platform wayland inject-test`. After the explicit terminal confirmation, approve the portal picker locally. UKVM submits only approximately +100 and -100 horizontal pointer motion. **The person at io must observe the pointer move**; an API success message alone is not proof. This does not click, scroll, or type.

Only after visible local movement, on io explicitly grant spectrum control and run the receiver:

```sh
target/debug/ukvm peer allow spectrum control-this-node
target/debug/ukvm listen --remote --bind 0.0.0.0:24800 --verbose
```

On spectrum, verify the configured right-edge peer is io, then run:

```sh
target/debug/ukvm layout
target/debug/ukvm control right
```

The receiver requests local RemoteDesktop consent before the controller first captures input. The experimental listener retains that grant until it exits; the second physical session on the same listener skipped the picker and completed a natural return. It releases held keys/buttons and resets remote pointer state between sessions. It does not request or store a portal restore token; restarting the listener asks again. Both computers must use the same Input Protocol 1.2 revision. Run the receiver with `--verbose` to show CreateSession, SelectDevices, Start/consent, ConnectToEIS, EI readiness, and advertised EI regions. Cross spectrum's right edge, observe pointer motion on io, then test buttons, scroll, and keyboard in a harmless field. Move inward on io, then intentionally cross io's left edge to request return. Spectrum should Release InputCapture near its right edge and restore local control. The remote control session has no 30-second active limit. Ctrl+Alt+Shift+Escape and Ctrl+C remain safety exits. On a rejected/unavailable receiver, controller capture never starts. Any network or injector loss releases capture and ends the receiver session.

io's diagnostic `hyprctl monitors` reported one logical `1366x768` eDP-1 display at `(0,0)` with scale 1. UKVM uses the EI pointer device's runtime regions, not this fixed diagnostic geometry. A zones-only portal query on io previously failed during cleanup because UKVM sent Disable before any EIS connection; the zones command now closes that unarmed session directly. Repeat it on the published build to verify the repair.

The first physical edge-return run on Input Protocol 1.2 succeeded: io's pointer appeared near its left edge and followed spectrum, io sent a left-edge return at fraction 0.39 after 72 pointer events, spectrum's portal Release succeeded, and the user confirmed local input control returned. A later run physically verified pointer, buttons, scroll, and keyboard input, followed by natural left-edge return. The *next* connection repeatedly authenticated but stopped before `CreateSession`, with no picker prompt. io's verbose log ended at `RemoteDesktop: opening portal interface`. Independent D-Bus `Get` and `GetAll` calls to the RemoteDesktop frontend both succeeded during the stall, so the portal frontend itself was responsive. Source inspection found that `ashpd::RemoteDesktop::new()` uses a process-global D-Bus connection, while UKVM had created that connection in a per-session injector Tokio runtime which ended after return. Opening a dedicated session-bus connection for each injector was not enough. Keeping the actor/runtime alive across sessions was also not enough: the second portal proxy construction still stalled. UKVM now constructs one RemoteDesktop proxy when the listener's injector actor starts and reuses it for separate permissioned sessions. On that revision, three consecutive physical spectrum → io → spectrum round trips succeeded on the same listener, with the user's confirmation that local input returned each time. The receiver's portal authorization is awaitable so Ctrl+C can interrupt a pending request; cancellation closes the pending portal session. A headless regression test covers authorization interruption.

On the later listener-lifetime grant revision, the first physical session requested portal consent and returned naturally; the next session on that same listener showed no picker and also returned naturally. A separate physical emergency test ended via Ctrl+Alt+Shift+Escape before reaching io's return edge, and the user confirmed spectrum's local mouse and keyboard worked immediately. For receiver-side disconnect cleanup, a timed abrupt stop of spectrum's sender was arranged in advance so the user could hold Shift and the left mouse button without needing to authorize a command while holding them. Before termination, the sender reported one keyboard event and one button event, with both still held through further pointer motion. io then logged `Remote session released: peer closed connection without sending TLS close_notify`; the user tested a harmless click and key locally on both PCs and confirmed normal control with no held state. A forced process stop cannot itself execute spectrum's normal portal `Release` path; the observed recovery is compositor cleanup on process exit plus io's peer-loss handling. No typed key content was logged.

The entry/return model uses an EI absolute pointer when the controlled portal advertises that capability. The receiver places the pointer 24 logical pixels inside the selected outer edge, maps the controller's portal activation fraction along the edge, and drives later motion with EI absolute requests. Return is armed only after at least 64 logical pixels of inward movement and requires outward motion across the same external boundary. This avoids immediate bounce. The XDG RemoteDesktop portal exposes EI regions but no authoritative compositor cursor position; pointer constraints or compositor transforms could make the visible cursor diverge from UKVM's requested position. UKVM does not use Hyprland IPC, XWayland or privileged devices for this mechanism. If the EI device lacks absolute pointer support, the remote-entry session fails safely.

The EI keyboard events currently carry Linux evdev physical codes. This first Linux-to-Linux proof assumes compatible keyboard layouts; cross-layout translation is not implemented.

## Rollback

Stop the receiver and any active capture, then on io run:

```sh
cd ~/UKVM
scripts/wayland-portal-trial stop
target/debug/ukvm doctor
```

The script verifies ownership and exact saved drop-in content, removes only its named persistent (or recognized legacy runtime) overrides, reloads the user manager, and attempts both packaged portal restarts in backend/frontend order using the current session environment. It verifies both services active before removing the private recovery marker and copied helper/backend/picker. If restoration fails, it reports nonzero and retains recovery data so `stop` can be retried. Modified/unrelated overrides are preserved and reported. Confirm that InputCapture remains available and RemoteDesktop returns to unavailable. The `control-this-node` permission is separate persistent UKVM trust state; if no longer wanted, run `target/debug/ukvm peer deny spectrum control-this-node` on io. No package uninstall or system configuration restoration is needed because none was changed.

If the script cannot finish rollback, inspect `systemctl --user cat xdg-desktop-portal-hyprland.service xdg-desktop-portal.service` and remove only the two `ukvm-trial.conf` drop-ins shown by `scripts/wayland-portal-trial status`, then `systemctl --user daemon-reload` and restart those two services. Persistent trial overrides survive reboot; use `stop` for complete removal.
