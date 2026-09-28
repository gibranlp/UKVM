# Milestone 12 Wayland injection trial

This is an **experimental, user-session-only** trial for io. Local pointer injection and two-node pointer, button, and keyboard input have been physically observed on io. Scroll and receiver-disconnect cleanup remain unproven. Keep the packaged portal available for rollback. Do not use the older generic portal revision: its inspected RemoteDesktop `Start` did not mediate user consent.

## Current evidence and dependency

On spectrum, Hyprland 0.56.2-3, xdg-desktop-portal 1.22.1-2, xdg-desktop-portal-hyprland 1.4.1-2, xdg-desktop-portal-gtk 1.15.3-1, and libei 1.6.0-1 were installed. `ukvm doctor` found InputCapture v1 with pointer and keyboard, but no RemoteDesktop. The packaged `hyprland.portal` does not advertise RemoteDesktop. io is expected to have the same SpectrumOS package set, but its own runtime versions and portal state must be checked before the trial.

The candidate is Hyprland's [draft xdg-desktop-portal-hyprland PR #426](https://github.com/hyprwm/xdg-desktop-portal-hyprland/pull/426), pinned at `0204ffbbcb3cef72af76804cdf59f114f2e2f6d9`. Its source was inspected and built on spectrum and io. The draft adds RemoteDesktop v2 and `ConnectToEIS`, a local consent picker, pointer/button/scroll and keyboard EI devices, and retains InputCapture. The backend uses compositor virtual pointer/keyboard protocols within the portal grant; UKVM itself only connects to the portal-provided EIS socket. The draft remains unmerged. Local pointer motion and cross-PC pointer, button, and keyboard input were observed with UKVM on io; scroll requires a physical test.

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
scripts/wayland-portal-trial start /tmp/ukvm-xdph-pr426 /tmp/ukvm-xdph-pr426-build
scripts/wayland-portal-trial status
target/debug/ukvm doctor
```

The script copies the installed portal descriptors and the effective `hyprland-portals.conf` into `$XDG_RUNTIME_DIR/ukvm-xdph-trial/portals`, replacing only that **copy** of `hyprland.portal` with the pinned draft. It adds two runtime systemd user drop-ins: `xdg-desktop-portal.service` gets `XDG_DESKTOP_PORTAL_DIR` pointing to the copied descriptors/configuration; `xdg-desktop-portal-hyprland.service` runs the built draft and puts its consent picker on `PATH`. It restarts those two user services. No packaged binary, system portal configuration, user trust record, private key, or permanent portal override is edited. The unchanged copied `hyprland-portals.conf` continues routing InputCapture to Hyprland. On reboot, runtime drop-ins disappear.

Before continuing, `doctor` must still show InputCapture with pointer capture and must now show RemoteDesktop v2, pointer, keyboard, and `ConnectToEIS`. If InputCapture disappears or the services fail, roll back immediately.

## Local injection proof, then remote proof

On io, run `target/debug/ukvm platform wayland inject-test`. After the explicit terminal confirmation, approve the portal picker locally. UKVM submits only approximately +100 and -100 horizontal pointer motion. **The person at io must observe the pointer move**; an API success message alone is not proof. This does not click, scroll, or type.

Only after visible local movement, on io explicitly grant spectrum control and run the receiver:

```sh
target/debug/ukvm peer allow spectrum control-this-node
target/debug/ukvm listen --remote --bind 0.0.0.0:24800
```

On spectrum, verify the configured right-edge peer is io, then run:

```sh
target/debug/ukvm layout
target/debug/ukvm control right
```

The receiver requests local RemoteDesktop consent for the session before the controller captures input. Cross the right edge, observe pointer motion on io, then test buttons, scroll, and keyboard in a harmless field. Ctrl+Alt+Shift+Escape on spectrum releases capture; the active session also ends after 30 seconds. On a rejected/unavailable receiver, controller capture never starts. Any network or injector loss releases capture and ends the receiver session. The trial does **not** implement seamless edge return.

The EI keyboard events currently carry Linux evdev physical codes. This first Linux-to-Linux proof assumes compatible keyboard layouts; cross-layout translation is not implemented.

## Rollback

Stop the receiver and any active capture, then on io run:

```sh
cd ~/UKVM
scripts/wayland-portal-trial stop
target/debug/ukvm doctor
```

The script removes only its marked runtime directory and named runtime drop-ins, reloads the user service manager, and restarts the packaged portal backend and frontend. Confirm that InputCapture remains available and RemoteDesktop returns to unavailable. The `control-this-node` permission is separate persistent UKVM trust state; if no longer wanted, run `target/debug/ukvm peer deny spectrum control-this-node` on io. No package uninstall or system configuration restoration is needed because none was changed.

If the script cannot finish rollback, inspect `systemctl --user cat xdg-desktop-portal-hyprland.service xdg-desktop-portal.service` and remove only the two `ukvm-trial.conf` runtime drop-ins shown by `scripts/wayland-portal-trial status`, then `systemctl --user daemon-reload` and restart those two services. Reboot also clears runtime overrides.
