# Native platform implementation plan

Planning date: 2026-10-14. This is an implementation proposal, not a claim of
additional platform support. The user has Windows and Apple Silicon Mac hardware
for physical testing; no native X11 test desktop is currently available.

Implementation order: shared foundations → Windows → macOS Apple Silicon →
clipboard/files/remote-session parity on those platforms → Linux X11 → final
interoperability acceptance. X11 is the last native platform implementation.
Milestone numbers below extend M21 and describe proposed work only.

## M22 implementation progress

The first foundation slice extracts the existing exclusive focus gate and
prepared input-session leases into `ukvm-runtime::session`, and cooperative
cancellation/retry pauses into `ukvm-runtime::lifecycle`. Wayland uses these
shared modules; portal/EI preparation, capture and injection stay in its adapter.
Cancellation ignores false watch updates and stops on either a true stop request
or a lost stop channel. Existing session/focus tests remain backend independent.

`native-platforms.yml` adds Linux, Windows MSVC and Apple Silicon macOS build,
test-compilation and Clippy jobs. Linux executes the full workspace suite;
Windows/macOS execute core, protocol, config, platform and shared runtime tests.
The workflow compiles all workspace tests on each target. Native network/CLI state tests are
not yet executed: secure storage, durable pairing records and OS management
still need native implementations. The native jobs establish compiler/test
evidence only, not input capability, user grants or physical acceptance.
Wayland test dependencies are now Linux-scoped.

Local validation passed all five required Linux workspace checks. Cross-target
`cargo check --all-targets` passed for core, protocol, config and platform on
Windows GNU and Apple Silicon macOS, with Clippy warnings denied on both.
The entire Windows GNU workspace also passed Clippy and compiled/linked every
workspace test executable. This exposed and fixed Linux-only CLI calls and
imports; unsupported native `control`, `listen --remote` and `run` fail before
creating identity/state, with a native-only CLI regression scheduled in CI.
Full Apple workspace cross-checking is blocked by the lack of an Apple SDK for
the existing TLS dependency's C compilation. These are Linux-hosted compiler
checks; native MSVC/Apple CI and physical testing provide separate evidence.
Hosted CI results have not been inspected: this workspace's GitHub CLI has no
API authentication, and the public repository API returns 404.

Remaining M22 work includes reusable readiness/routing orchestration, backend
session contracts, physical-key mapping, native ownership/private storage,
backend selection and cross-platform management status. Windows/macOS input and
X11 are still unimplemented. No protocol version or stored portal credential
changes are part of this slice.

## Outcome and scope

The first release gate is universal physical KVM: every supported installation
can capture local keyboard/pointer input and receive authorized peer input, using
the same identity, pairing, permissions, topology and focus model. Each computer
keeps its own monitor. There are no separate controller/controlled installers.

A second gate is feature parity: opt-in plain-text clipboard, secure files,
interactive remote viewing/control, daily startup and management status. Report
these capabilities individually until each passes on each platform. Native KVM
support alone does not establish parity with all existing Wayland features.

Preserve physically accepted Wayland input, portal persistence and clipboard
behavior. Existing M14 geometry limitations, M20 pending physical acceptance and
M21 pending performance comparison remain separate; this plan does not close
those acceptance items. Do not reset saved portal credentials during testing.
Audio, recording, headless outputs, richer clipboard, drag/drop, arbitrary remote
commands and a new general-purpose GUI are outside this plan.

## Repository findings

- `ukvm-platform` has capture, injector, display and capability interfaces, but
  `backend_status` reports X11/macOS/Windows unimplemented. The generic capture
  interfaces are not yet the working runtime integration path.
- `ukvm-runtime::run` selects `wayland::run` on Linux and rejects other targets.
  Outbound readiness, incoming sessions, clipboard and file orchestration live
  partly under `runtime::wayland`; adding an injector alone will not deliver KVM.
- Runtime ownership uses Linux-specific Unix metadata/runtime-directory rules.
  Service startup validates a Wayland session. Both need native equivalents.
- `KeyCode` currently has only `LinuxEvdev(u32)`. Physical-key translation across
  Windows scan codes and macOS virtual key codes needs an explicit contract.
- Native remote capture/viewer and their Cargo dependencies are Linux/Wayland
  specific. Existing native window code does not establish Windows/Mac support.
- File publication is Linux-specific. Unix private modes in identity/trust code
  do not establish adequate private storage protection on Windows.

Keep concrete native operations in `ukvm-platform`; reuse core, protocol,
network and discovery rather than creating parallel implementations. Audit all
workspace target guards and native dependencies, not only these entry points.

## M22 — Shared runtime and portability foundation

1. Record the existing Wayland regression baseline and current protocol versions
   (Input 1.4, Pairing 2, Clipboard 1, File 1, Display 2). Add native Windows and
   Apple Silicon macOS CI build/test jobs alongside Linux. Select minimum OS
   versions after small API/build probes; report the tested version ranges.
2. Extract reusable readiness/retry, incoming authorization, exclusive focus,
   source-owned topology routing, held-state handoff and cooperative shutdown
   from the Wayland runtime in small reviewed changes. Keep portal/EI lifecycle
   in a Wayland adapter. Use bounded actors for native APIs requiring an OS
   thread/run loop; callbacks must never wait on TLS, disks or codecs.
3. Define the backend contract around actual needs: session readiness and loss,
   outer-edge geometry, capture activation/suppression/release, native pointer
   observation, input injection, display changes and local override. Adapt the
   working Wayland backend first; avoid a wholesale rewrite.
4. Specify physical-key, button, scroll and coordinate semantics. Start by
   mapping the supported physical keys into the existing explicit evdev domain
   on each backend; do not send text characters or assume raw native numbers
   match. Cover left/right modifiers, AltGr, ISO/JIS keys, extended keys, repeat,
   locks and Command/Option/Windows keys. Report unmappable keys honestly.
   If the current domain cannot express required keys, review/version the input
   protocol before extending it; old peers must reject incompatible versions.
5. Define native per-user ownership, private advisory status and shutdown
   adapters. Review state locations, edit locks, permissions, durable atomic
   replacement and symlink/reparse-point safety. Windows needs explicit user
   ACL protection; macOS needs private user storage and stable app identity.
6. Keep frontend JSON schemas compatible, exposing unavailable capabilities and
   permission failures. `doctor` remains read-only and requests no OS grants.
   Backend detection must distinguish native Xorg from Wayland with DISPLAY;
   a Wayland failure must never silently choose XWayland.

Exit: native CI compiles the workspace with honest unsupported capabilities;
Wayland passes the existing runtime tests and physical switching/release smoke
test without re-pairing or reauthorizing its saved portals.

## M23 — Windows native KVM and daily runtime

Target the user's Windows PC and its logged-in interactive desktop first.
Use established Rust Windows bindings, reviewing feature set, license and MSRV
before adding dependencies.

- Build an explicit opt-in local probe: monitor enumeration, pointer readback,
  capture, suppression and injection. No network input until this is proven.
- Evaluate low-level keyboard/mouse hooks for suppression while controlling a
  peer; Raw Input can supplement device observation but does not replace the
  suppression requirement. Run the message loop on a dedicated thread with
  bounded queues. Detect loss of capture health and restore local input.
- Inject via `SendInput`, checking the inserted count and handling partial
  failure. Map physical scan codes including extended keys; mark owned injected
  events and prevent them from re-entering outbound capture. Test this when the
  same node has both roles available.
- Map per-monitor DPI, negative origins and virtual-desktop absolute coordinates.
  Use native geometry to exclude internal seams; revalidate after display/DPI
  changes. Edge detection and cursor confinement/parking require a measured
  prototype, with cleanup of every native resource on all exit paths.
- Integrate the common runtime, permission guards, natural return and observed
  inward emergency rearm. Test local Ctrl+Alt+Shift+Escape while input is being
  suppressed. Release all UKVM-held state on loss of focus/session/authorization.
- Add user-session startup and single-instance ownership; do not put input in a
  Session 0 system service. Preserve explicit install versus enable/start choices.
  Handle lock, logout, sleep/wake and desktop transitions by ending active control.
- Package an executable/installer, preserve identity/trust on upgrade, document
  removal semantics, and add safe CLI status/service management. Signing is a
  release task dependent on the owner's signing credentials.

Windows boundaries are part of the support contract: ordinary UKVM cannot inject
into higher-integrity apps through UIPI. UAC secure desktop, sign-in desktop and
secure-attention sequences are outside this release. Do not elevate the
network-facing runtime to overcome them. Hook callbacks can be silently removed
after timeouts, so callback health/recovery is an acceptance requirement.
See Microsoft's [SendInput contract](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput)
and [hook lifecycle guidance](https://learn.microsoft.com/en-us/windows/win32/winmsg/lowlevelkeyboardproc).

Exit: Wayland ↔ Windows passes physical KVM acceptance in both source directions,
including startup/restart and permission denial; Windows ↔ Windows is separately
tested when a second independent Windows desktop is available.

## M24 — macOS Apple Silicon native KVM and daily runtime

- Produce a native arm64 build and stable `.app` identity early, so permission
  testing uses the intended executable identity. Review maintained Rust Apple
  bindings; isolate FFI with ownership/threading invariants.
- Use Quartz event taps for capture and authorized event filtering, Core
  Graphics event posting for injection and native display/pointer APIs for
  geometry. Prove suppression, cursor management and emergency release locally
  before enabling network capture.
- Probe Accessibility and Input Monitoring separately; request access only in
  explicit setup/operation. Denial, revocation and secure-input restrictions must
  fail closed with actionable status. A trusted peer cannot grant OS permission.
- Translate physical keys and modifier flags, test Command/Option and ISO/JIS
  mappings, and suppress owned injected-event loops. Handle event-tap disablement
  and run-loop teardown; never block an event callback on network work.
- Normalize Retina scaling, negative monitor origins and display reconfiguration
  for the shared edge/entry model. Native pointer readback must drive emergency
  rearm, rather than a requested injected position.
- Add per-user LaunchAgent management, ownership/status, cooperative shutdown and
  lock/logout/sleep/wake cleanup. Provide a signed/notarized app package for
  distribution when owner credentials are available, plus upgrade continuity for
  peer identity and permissions.

See Apple's [event-tap API](https://developer.apple.com/documentation/coregraphics/cgevent/tapcreate(tap:place:options:eventsofinterest:callback:userinfo:))
and [listen-access preflight](https://developer.apple.com/documentation/coregraphics/cgpreflightlisteneventaccess()).
The exact grant/filtering behavior must be verified on the user's macOS version.
Intel macOS remains untested until it has its own build and physical evidence.

Exit: Wayland ↔ Mac and Windows ↔ Mac pass in both source directions; removing an
OS grant stops control and restores local operation without changing peer trust.

## M25 — Windows/macOS feature parity

Deliver these independently after native KVM acceptance, in this order:

1. **Text clipboard:** native Windows Unicode clipboard notifications/ownership
   and macOS pasteboard changes/ownership, behind `ClipboardBackend`. Preserve
   separate bilateral opt-in, bounded UTF-8, empty text, live revocation and
   no third-peer relay. Test contention, OS clipboard-history interactions,
   line endings and echo prevention without logging contents.
2. **Secure files:** native `ukvm-platform::files` implementations preserving
   private staging, no-follow traversal, verified streaming, atomic no-replace
   publication and live permissions. Review Windows reparse points, reserved
   names, alternate data streams and ACLs; macOS symlinks and case/Unicode name
   collisions. Filesystem semantics require native tests, not emulated success.
3. **Remote sessions:** first port the viewer and codec pipeline, then native
   screen capture. Use Windows Graphics Capture with explicit system selection
   and macOS ScreenCaptureKit with screen-recording permission. Keep viewing
   independent from control permissions and reuse the existing input focus gate
   and injector. Port the private same-user runtime bridge (e.g. user-restricted
   named pipe on Windows); do not expose a generic command channel.

Preserve Display 2 frame/decoded-pixel/queue bounds and performance metrics,
keeping capture and decode off input/UI callbacks. Verify codecs and native
window dependencies on both targets. OS capture permission is separate from
`view-screen`; Wayland restore tokens have no role on these platforms.
See [Windows screen capture](https://learn.microsoft.com/en-us/windows/uwp/api/windows.graphics.capture)
and [Apple ScreenCaptureKit](https://developer.apple.com/documentation/screencapturekit).

Exit: each feature passes bidirectional interoperation with Wayland and between
Windows/Mac. Optional-feature denial or failure leaves physical KVM usable.

## M26 — Linux X11, last native platform

- Select X11 only for a verified native Xorg user session. Review a Rust X11
  binding such as `x11rb`; query XI2, RandR, XFixes and XTEST versions explicitly.
- Use XI2/native grabs for input capture and suppression, RandR for displays,
  XFixes barriers where available, and XTEST for input injection. Prove boundary
  activation and ungrab behavior rather than treating raw observation as capture.
  Missing extensions must report unavailable capabilities.
- Explicitly map XKB physical keys; do not assume X keycode minus eight works
  for all layouts/devices. Exclude UKVM's injected events from outbound capture,
  with physical testing of simultaneous controller/controlled availability.
- Integrate the common runtime, actual pointer observation, outer-edge geometry,
  natural return, emergency hysteresis and display-change cleanup.
- Extend Linux session startup to native Xorg without weakening Wayland session
  validation or importing unrestricted environment variables. X server loss,
  grab contention, lock and logout must release control.
- Add regular `CLIPBOARD` selection ownership with bounded UTF-8/INCR handling;
  reuse Linux file storage. Port X11 viewer features, then implement explicit
  local monitor-sharing approval before any X11 pixels are read or transmitted.
  X server access alone is not UKVM screen-sharing approval.

X11 has weaker same-desktop isolation than Wayland; UKVM still enforces pinned
TLS and default-deny peer permissions. Use the user's normal authenticated X
connection, never `xhost +`, root input access or a Wayland fallback.
See [XTEST](https://www.x.org/releases/X11R7.5/doc/man/man3/XTestFakeKeyEvent.3.html)
and [XFixes](https://www.x.org/releases/current/doc/fixesproto/fixesproto.txt).

Exit: native Xorg ↔ each accepted platform passes in both source directions.
Xvfb/nested X servers can assist automated checks; a VM with a native Xorg desktop
can assist physical testing, but requires its own consent/setup and does not
substitute for all display-manager/multi-monitor evidence. Until such a desktop
is available, list X11 as implemented/untested rather than accepted.

## M27 — Universal release acceptance

For four backends, track all six unordered platform pairs in both source
directions. Also track same-backend pairs separately as hardware permits.
For every implemented feature, record OS/session/version, build/commit and
observed evidence; CI success alone never marks native operation accepted.

Required physical KVM cases for each pair:

- At least ten handoff/return cycles, pointer/buttons/both scroll axes, typing,
  repeat and left/right modifiers; held key/button across handoff and disconnect.
- Emergency release with near-edge/tangential movement unable to rearm; observed
  inward movement allows deliberate reentry. No injected-event feedback loop.
- Offline/untrusted/denied/busy peer, live revocation, receiver restart, source
  graceful shutdown, abrupt process loss and network partition. Check the
  documented health deadline and local recovery; do not promise instantaneous
  cleanup after a crash that prevents the injector from running.
- Login startup, duplicate instance, lock/unlock, logout, sleep/wake and display
  hotplug. Multiple displays with offsets, negative origins and mixed DPI; test
  internal seams and more than one external direction where hardware permits.
- Three-node source-owned routing across different platforms: each destination
  independently authorizes the physical source, and only one lease injects.

For clipboard/files/viewing, run denial/revocation, malformed data, interrupted
operation and reconnection cases in addition to successful transfer/control.
Compare remote performance on actual hardware; do not infer parity from synthetic
codec measurements. Schedule a measured endurance run and record its duration.

For each code milestone run the repository's required formatting, check, test,
Clippy and build commands on applicable native targets. Add meaningful native
mapping/lifecycle/storage tests and retain existing protocol/security tests.
Before requesting another-PC tests, review staged files for secrets/build output,
commit with the owner's identity, push, verify success and report the commit.
After validated runtime changes, redeploy on the authorized Spectrum PC following
`AGENTS.md`; other machines' installation/startup and OS grants remain explicit.

Update README, platform support, security, service and feature docs to distinguish
implemented, OS-authorized and physically accepted capabilities. Publish native
release artifacts and checksums, tested OS versions, installation/upgrade paths
and remaining restrictions. Do not label untested architectures as supported.

## Execution and dependencies

Start with M22 and a small Windows local capture/suppression/injection probe.
Keep each milestone split into reviewable changes: build/probe → native backend
→ shared runtime integration → physical acceptance → packaging. Proceed to the
next platform only after recovery behavior on the current one is understood.

Available physical hardware supports Windows and Apple Silicon first. Exact OS
versions, multi-monitor coverage, signing credentials and a native Xorg test
desktop are release dependencies to resolve during the relevant milestone.
Calendar estimates should follow the first native suppression/geometry probes;
these and secure-storage behavior are the largest current uncertainties.
