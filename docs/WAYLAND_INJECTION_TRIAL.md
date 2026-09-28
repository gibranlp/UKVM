# Milestone 12 Wayland injection trial

This is an **experimental, user-session-only** trial for io. Real input injection has not yet been physically observed. Keep the packaged portal available for rollback. Do not use the older generic portal revision: its inspected RemoteDesktop `Start` did not mediate user consent.

## Current evidence and dependency

On spectrum, Hyprland 0.56.2-3, xdg-desktop-portal 1.22.1-2, xdg-desktop-portal-hyprland 1.4.1-2, xdg-desktop-portal-gtk 1.15.3-1, and libei 1.6.0-1 were installed. `ukvm doctor` found InputCapture v1 with pointer and keyboard, but no RemoteDesktop. The packaged `hyprland.portal` does not advertise RemoteDesktop. io is expected to have the same SpectrumOS package set, but its own runtime versions and portal state must be checked before the trial.

The candidate is Hyprland's [draft xdg-desktop-portal-hyprland PR #426](https://github.com/hyprwm/xdg-desktop-portal-hyprland/pull/426), pinned at `0204ffbbcb3cef72af76804cdf59f114f2e2f6d9`. Its source was inspected and built on spectrum. The draft adds RemoteDesktop v2 and `ConnectToEIS`, a local consent picker, pointer/button/scroll and keyboard EI devices, and retains InputCapture. The backend uses compositor virtual pointer/keyboard protocols within the portal grant; UKVM itself only connects to the portal-provided EIS socket. This is unmerged experimental code and has not yet been proven with UKVM or on io.

## Build on io, without installation

In io's graphical user session, with the Milestone 12 UKVM revision pulled and built:

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
target/debug/ukvm listen --remote
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
