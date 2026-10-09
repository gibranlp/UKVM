# Linux/Hyprland package integration

## Upstream evidence checked 2026-10-08

[Hyprland XDPH PR #426](https://github.com/hyprwm/xdg-desktop-portal-hyprland/pull/426) is open, draft and unmerged. Its head remains `0204ffbbcb3cef72af76804cdf59f114f2e2f6d9`. The [latest official release v1.4.1](https://github.com/hyprwm/xdg-desktop-portal-hyprland/releases/tag/v1.4.1), revision `cc8e5ef8fb2acef3db488b9a33b0c48c2a4ee204`, published 2026-07-29, does not provide the RemoteDesktop interface UKVM needs. Official master `e87ae7823e7bf0601385220c69b5a3b245123fc5` (2026-09-22) still contains InputCapture and no RemoteDesktop portal implementation. The PR's RemoteDesktop CreateSession/SelectDevices/Start/ConnectToEIS v2 API remains the tested API; no UKVM input-protocol change is needed.

This is a downstream production packaging candidate, not an upstream-supported release. SpectrumOS temporarily packages the exact physically tested upstream PR revision, with its `hyprland-protocols` submodule at `bd153e76f751f150a09328dbdeb5e4fab9d23622`. No additional compositor/input patch is applied. Normal users install signed distro binaries; the developer trial remains separate. Replace the downstream package with a tested official release once equivalent support ships. Recheck consent behavior, API properties and physical acceptance before changing the pin.

## Package responsibilities

UKVM stays distro independent. Its normal user service and runtime are unchanged. The `ukvm` package stages `/usr/bin/ukvm`, the vendor user unit and docs. It also stages the independently invoked `/usr/lib/ukvm/ukvm-portal-session` helper for portal packaging; merely installing UKVM does not change another desktop's portals or start any service.

SpectrumOS's explicitly named `spectrum-xdg-desktop-portal-hyprland` package provides/conflicts with the ordinary XDPH package through pacman. Its package-owned binaries, portal interface definition and vendor user-unit drop-ins live under `/usr`, never `/tmp` or an individual user's runtime directory. It is Hyprland-specific. Other desktops should use the independent `ukvm` package with their native supported portals, without the SpectrumOS Hyprland portal package.

The backend keeps its DBus service type/name. Its wrapper resolves one active local logind Wayland session, authenticates the current compositor/socket using owner, process start time and Hyprland instance, and uses that session's runtime, display and bus. Missing or ambiguous sessions fail with exit 75. Clearing inherited `ConditionEnvironment` prevents a permanently skipped login; systemd retries failure every five seconds, with rate limiting. Backend ExecStartPost checks its unique DBus owner's environment and required keyboard/pointer interfaces. Frontend activation binds to and starts the backend, orders after its readiness check, and independently validates the frontend. Restarting the backend propagates to the frontend; backend Wants ensures the frontend is started again. Logout stops both through graphical-session.target. A new DBus activation starts them against the new session even when the manager/runtime directory was recreated. There are no polling daemons, shell-profile hooks or input-runtime portal restart workarounds.

Portal activation still follows normal DBus/systemd lifecycle; UKVM's existing readiness check activates the frontend when needed. Vendor drop-ins persist across user-manager/runtime recreation. User overrides retain systemd precedence and are not deleted or silently overridden. Migration from a developer trial requires explicitly stopping that owned trial once before activating the package. Installation/removal never deletes user trust, identities, configuration or unrelated overrides. Package installation does not force-restart running portals or UKVM: log out/in after switching the package.

## Consent and limitations

Portal consent remains explicit. The upstream RemoteDesktop Start path prompts for device-specific local approval unless the portal itself validates a previously user-approved restore grant. UKVM does not supply or store a restore token and still requires fresh consent after its listener restarts. Packaging never selects Allow or requests persistent approval. InputCapture remains permissioned as before. A read-only doctor report proves advertised interfaces only.

Hyprland's partial external-edge barrier validator limitation remains unchanged; use a normal full external edge for acceptance. No privileged input, security bypass, compositor workaround or automatic trust is added. Actual clean-install, logout/login, reboot and physical control acceptance of this production package remain required; M15/M17 trial results do not establish those claims for these packages.

See SpectrumOS `docs/ukvm-production.md` for the distro build/release and physical acceptance procedure.
