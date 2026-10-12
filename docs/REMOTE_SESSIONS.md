# Interactive Remote Sessions — performance follow-up

`ukvm remote PEER` opens a native Wayland window for a pinned peer. The selected
monitor appears after the peer approves its normal ScreenCast picker. Click the
rendered desktop to request control; there is no physical edge crossing. The
status strip shows the peer, connection state, click-to-control/controlling state
and Ctrl+Alt+Shift+Escape. That shortcut releases input while keeping viewing
open. Click again to request a new control lease. Losing window focus also
releases control. Closing the window, display/input failure, permission loss,
backend loss and runtime shutdown clean up held input.

The user physically confirmed live viewing, mouse/keyboard/click/scroll,
ScreenCast restoration across reconnect and reboot, and clean return to normal
physical topology when closing the window. The performance follow-up described
here has automated/local synthetic evidence only; perceptual improvement and
real two-PC pipeline timings await the user's comparison. Existing M18–M20
evidence is unchanged. See [performance measurements](REMOTE_PERFORMANCE.md).

## Architecture and ownership

The platform capture backend uses a private D-Bus connection, official
ScreenCast CreateSession/SelectSources/Start/OpenPipeWireRemote, and the returned
restricted PipeWire FD. It requests one monitor with an embedded cursor and
official ScreenCast v4+ persistence until explicitly revoked. Its returned
single-use restore token uses the existing private authorization store under its
own ScreenCast record, separate from RemoteDesktop/InputCapture. The first local
approval remains mandatory; when the backend honors saved consent, subsequent
connections restore the selected monitor without another picker. Backend
revocation, missing outputs or unavailable persistence require normal consent. Consent cancellation/disconnect/shutdown
closes that private connection and the session; capture and codec actors are
joined. It neither alters nor resets M18's RemoteDesktop/InputCapture credentials.
No external capture/codec programs, direct compositor capture, root input access,
XWayland fallback or virtual/headless output creation is used.

PipeWire maps packed BGRx/BGRA/RGBx/RGBA buffers. Capture copies validated
stride, offset and size data into one replaceable raw slot. A dedicated codec
actor wakes on publication and uses libjpeg-turbo's SIMD encoder directly on the
packed pixels, without intermediate RGB conversion/downscaling. Defaults are
native source resolution, JPEG quality 97, full 4:4:4 chroma, up to 60 FPS and a
500 Mbps JPEG-payload video ceiling. The viewer can lower FPS, quality and
bitrate explicitly. Incompressible frames try lower quality (80/65/45), retaining
geometry/chroma, to fit the 8 MiB JPEG bound. Unsupported/bound-exceeding capture
fails the display session safely.

The viewer validates JPEG dimensions before allocating at most 64 MiB of pixels,
limits progressive scans, and decodes directly into the softbuffer pixel format.
Decode and bilinear scaling run on a dedicated worker with latest-only input and
output slots. Native-size scaling shares the decoded pixel storage. Coalesced
winit notifications submit ready frames promptly without an unbounded GUI event
queue. The window requests native content size when it fits the local monitor;
the compositor may override that request. Letterboxing and the 48-pixel status
header remain independent of input mapping. Status refreshes have bounded timer
wakeups; baseline mode retains the previous 33 ms refresh cadence.

The PipeWire format offer includes native sizes up to 8192 per dimension, with
at most 16,777,216 pixels, and accepts fixed refresh up to 1000 Hz or damage-only
streams. Copies are throttled to the requested FPS; a refresh range is required
because many monitor nodes advertise a fixed native rate. Negotiation follows
the official [PipeWire video capture example](https://docs.pipewire.org/page_tutorial5.html).

Clicking connects to the local runtime's private, same-user Unix input bridge.
That runtime reserves its existing focus gate, closes any idle physical capture,
and creates its usual pinned input Sender to the selected peer. An active physical
relationship or competing incoming/window lease refuses the click. The receiver
uses the existing ExclusiveSink → SharedSession → RemoteInputSink →
WaylandInputInjector → permissioned RemoteDesktop/EIS actor. `WindowEntered`
confirms actual exclusive entry before the UI reports controlling. Buttons,
physical evdev keys, scroll, heartbeats and held-state cleanup use the existing
input path. There is no VNC input transport, independent injector or edge-return
state in a window lease. Ending it restores the gate for physical switching.

A GUI release watch has priority over its bounded event queue. Events carry a
local generation so previously queued input cannot reach a new lease. Closing
its Unix connection makes the runtime prioritize SessionEnd over queued input;
network EOF remains the cleanup path after crashes. The bridge is available only
while the normal user runtime owns the permanent runtime lock; it accepts no
commands, trust changes, service actions or permission edits. At most four local
requests coexist. The UI can view without a local runtime but cannot control.

Platform-neutral display messages, frame bounds, target geometry and viewport
math are separate from Linux portal, PipeWire and window code. A future native
capture backend or encoder can use the same seams; no Windows/macOS backend,
audio, recording, multiple viewing monitors or automatic headless setup exists.

## Display protocol and security

`ukvm-display/2` is a separate ALPN connection on the existing runtime TCP
listener. It uses the same TLS 1.3 identity, exact certificate pins and connection
permission. The viewer verifies the authenticated certificate ID. Discovery and
aliases remain address/name hints, never authorization. No pixels use the input,
clipboard or file-transfer channel.

Display v2 is a fixed-integer bincode protocol with four-byte big-endian
length framing, bounded to 8 MiB + 1024 bytes. Messages are Hello(version=2),
Configure(bounded video preferences), Ready(interactive permission), Frame and
Ack(sequence, monotonic receive/send timestamps). Frames include only numeric
performance metadata in addition to the existing geometry and JPEG. JPEG is
capped at 8 MiB; encoded dimensions at 8192 each and 16,777,216 pixels; mapping
IDs at 128 bytes; logical geometry at bounded 16384-pixel spans and ±32768
coordinates. Bad versions/lengths/trailing data, nonfinite capture rates,
JPEG metadata, sequence regressions, rates and unexpected messages fail the
session. Pixel storage is reference-counted locally to avoid deep frame clones;
its wire representation remains an ordinary bounded byte vector. Debug output
omits pixel bytes. No screen contents, input events or secrets appear in metrics.

There is one display capture slot across peers, one pending raw/encoded/decode/
prepared frame at each stage and one acknowledged frame in flight. New frames
replace stale ones; consumers skip missed deadlines. A stalled socket write
closes within one second; reads/acks within five. Consent has a 120-second server
limit. Receiver rate checks allow at most 75 frames/second around the 60 FPS
target. Source pixels and render allocations remain bounded to 64 MiB each.
Damage-only sources repeat their latest image once per second for liveness;
those old images have correspondingly old capture ages.

Video pacing accounts for network transmission time once. Backpressure reduces
sent FPS and drops superseded video before it can build a backlog. When an
exchange exceeds the requested FPS period, pacing leaves about 10% additional
headroom instead of continuously saturating the video socket. Socket send
buffers are limited, and Linux applies a small not-sent threshold and best-effort
CS1 lower-priority marking. Input uses its existing separate sockets, focus gate,
event queue and injector. Codec work and display serialization/deserialization
run off the input executor; input never waits for frame decoding/presentation.
The local window-input bridge keeps one periodic policy timer instead of doing
an extra immediate policy read for every pointer message. Actual wire priority
still depends on host/router queue disciplines; saturated shared hardware cannot
provide an unconditional QoS guarantee. Native 4K CPU JPEG can remain expensive
and bandwidth-limited. H.264/hardware paths are future work and require explicit
codec negotiation; no such path is claimed by this release.

Both peers must update for Display v2; older display channels fail closed.
Input 1.4, Pairing 2, Clipboard 1, File 1 and all consent/trust records remain
unchanged. `--baseline` emulates the earlier 1080p/30 FPS quality-92 software JPEG,
GUI decode/scale and post-ack pacing within Display v2 for controlled comparison.
It is not byte-for-byte replay of the old executable.
The viewer allows twelve seconds for its first frame after Ready, matching the
server's ten-second usable-buffer deadline; subsequent reads use five seconds.

`view-screen` is a new independent receiver-side permission, default OFF,
including missing fields in old records. It also requires `connect`. Viewing
neither grants nor requires control/clipboard/files permissions. Interactive
requests require both view-screen and control-this-node and normal RemoteDesktop
consent. Viewing authorization is checked before any portal request, during
consent, periodically during sharing and under the shared policy lock before
publishing each frame. Revoking view-screen terminates viewing and window input;
revoking control-this-node releases input but leaves authorized viewing open.
The display sends changes in interactive permission to the window. Runtime input
checks control permission at its existing 250 ms interval; window viewing policy
is additionally checked on the input connection at 100 ms. Policy read failures
fail closed. Already delivered frames cannot be recalled from a viewer.

Input Protocol **1.4** adds WindowSessionStart, WindowEntry(target, position),
WindowEntered and PointerAbsolute. Other session sequencing and physical topology
behavior stay the same. Absolute input is rejected outside window sessions;
relative motion/physical topology entry is rejected within them. Both peers need
1.4; 1.3 fails at Hello. Pairing Protocol 2, Clipboard Protocol 1 and File Protocol
1 are unchanged. No re-pairing or identity replacement is needed.

## Coordinates and limitations

The viewport uses the current window's physical pixels and aspect-fit
letterboxing, excluding its 48-pixel header. Clicking bars/header does not start
control. Encoded pixel dimensions never replace the logical coordinate metadata.
Positions are converted to fractions within the selected stream. The injector
matches the portal mapping ID to an EI region and maps those fractions into
that region's logical coordinates, including differing origins/scales. If EI has
no mapping IDs, an exact known-origin rectangle or a single region with matching
logical dimensions is accepted. With a mapping ID, missing portal size can use
normalized unit coordinates. Without an ID, missing origin permits only the
single matching region; missing size is view-only. Ambiguous mappings or missing
EI absolute support reject control while leaving the display available. Mapping
IDs from independent ScreenCast/RemoteDesktop sessions are backend-dependent;
this needs physical validation. Output changes requiring a new portal/EI grant
require reopening the session. Negative stride, DMABUF-only and non-packed video
sources are unsupported and reported. There is no automatic reconnect; reopen
the window after display disconnection. The first click positions/activates
control; subsequent clicks operate the remote desktop.

## Dependencies and build

Existing ashpd/zbus/reis and rustls/tokio are reused. New Linux dependencies:
pipewire 0.9.2 (SPA/PipeWire bindings, MIT), winit 0.30 (native Wayland input/window,
Apache-2.0), softbuffer 0.4 (software presentation, MIT/Apache-2.0), jpeg-encoder
0.6 (MIT/Apache-2.0/IJG), jpeg-decoder 0.3 without Rayon (MIT/Apache-2.0), and
font8x8 0.3 (minimal status strip, MIT). The performance pass adds turbojpeg 1.5.1
(MIT/Unlicense) with system libjpeg-turbo >=3.0, bytemuck 1 (MIT/Apache-2.0/Zlib)
for safe pixel-slice casting, and socket2 0.6 (MIT/Apache-2.0) for bounded video
socket backlog. Pure Rust JPEG crates remain for explicit baseline comparison. Versions are pinned in Cargo.lock; declared
workspace MSRV remains Rust 1.87; this pass was validated with the installed stable toolchain. Build
requires PipeWire/SPA headers, pkg-config and libclang; runtime requires PipeWire,
a sanctioned ScreenCast backend, and a native Wayland session. On Arch/SpectrumOS,
the existing setup script includes clang, libpipewire, libjpeg-turbo and libxkbcommon. No codec executable
or external capture process is used.

## Two-PC physical test

After the published commit is available, in the checkout on **both** PCs:

```sh
git pull --ff-only
sudo pacman -S --needed libjpeg-turbo
cargo build --release --locked --workspace
./target/release/ukvm version
./target/release/ukvm install
export PATH="$HOME/.local/bin:$PATH"
~/.local/bin/ukvm service install
~/.local/bin/ukvm service restart
~/.local/bin/ukvm version
```

Use a graphical user terminal. Stop an existing foreground `ukvm run` cooperatively
before switching to the service; do not launch a second runtime. Installation
and unit installation alone do not start/enable it; `service restart` here is an
explicit test action. For an existing distro-owned production executable, this
installs the development binary in ~/.local/bin without overwriting /usr.
Both explicit version checks must show protocol 1.4. Use ~/.local/bin/ukvm for
the test if the shell still selects an older /usr/bin/ukvm.
Do not reinstall/reset the portal trial, identity, pairing or authorization state.

On **io**, using its already-paired alias for Spectrum (normally `spectrum`):

```sh
ukvm peer allow spectrum view-screen --json
ukvm peer allow spectrum control-this-node --json
ukvm peer permissions spectrum
```

The first change is live. If control permission is newly enabled, use the existing
`ukvm apply` flow; normal RemoteDesktop consent remains required. No other peer
permission is implicitly changed. Use the alias shown by `ukvm peers` if different.

On **Spectrum**:

```sh
ukvm remote io
```

Approve io's legitimate ScreenCast picker and select one monitor. Verify live
video, click-to-control, mouse, keyboard, clicks, scrolling and left/right
modifiers. Press Ctrl+Alt+Shift+Escape; confirm local input and continued viewing.
Click again, then close the window while a modifier/button is held and verify
both desktops have normal local input. Also test viewer termination/disconnect.

On io, while a session is active, test each independent revocation:

```sh
ukvm peer deny spectrum control-this-node --json
ukvm peer allow spectrum control-this-node --json
ukvm peer deny spectrum view-screen --json
ukvm peer allow spectrum view-screen --json
```

Control denial must release held input while viewing stays available; click again
after reallowing. Viewing denial must disconnect video and input; reopen after
reallowing. For view-only testing deny control before opening `ukvm remote io`.
Restore only the permissions you intended to retain. Finish with normal physical
edge switching and existing opt-in clipboard/file-transfer tests. No automated
check establishes physical success for these steps.

## Black window / display-channel failure diagnostics

The first two-PC attempt reported a black window with `io display channel closed`
and apparent desktop freezing, despite both CLIs reporting Input Protocol 1.4.
No desktop delivery/control success is claimed. Follow-up fixes bound redraw
scheduling, supply native size/refresh negotiation, align the first-frame
deadlines, and surface remote failures without verbose mode. Regression tests
cover redraw wakeups and the serialized capture offer. Workspace validation
passed (281 tests, zero failures, five opt-in tests ignored), including fmt,
check, Clippy with warnings denied, and debug/release builds. The user's failure
still requires receiver logs and a physical retest to establish its cause.

The viewer prints display failures to its terminal; the controlled runtime prints
capture and authentication/permission failures to its service journal even without
verbose mode. A generic `display channel closed` in the viewer does not identify
the remote cause. On the controlled PC inspect:

```sh
~/.local/bin/ukvm peer permissions spectrum
journalctl --user -u ukvm.service --since '20 minutes ago' --no-pager
```

Confirm that the service's executable is the intended build as well as the CLI:
`systemctl --user show ukvm.service -p ExecStart`. A first-frame failure remains
a display-only failure; it must not grab local input or stop physical KVM.
Report which PC stalls, whether the ScreenCast picker appeared, and the service
error. Do not reset trust or portal restore tokens to troubleshoot video.

## Historical Display 1 consent/quality retest (da69acf)

This records the earlier Display 1 build. The user subsequently confirmed
remembered ScreenCast sharing across reconnect/reboot and end-to-end control.
Display 2 bounds and performance comparison above supersede these image limits.

Local validation passed fmt, workspace check, tests (289 passed, zero failures,
five opt-in tests ignored), Clippy with warnings denied, and debug/release builds.
No new dependencies or input/display protocol versions were added by this follow-up.

Update both PCs with the commands above; Spectrum is redeployed automatically
by the development workflow. On io, the first connection after this update still
needs local ScreenCast approval. Select the intended monitor and choose the
portal's remember/skip-future-prompts option if offered. UKVM never selects that
choice for you. Then on io:

```sh
~/.local/bin/ukvm authorization status
```

Screen sharing must report Persistent (remembered). A missing token means the
backend did not persist consent; unattended viewing is not established. Close the
viewer, reopen `~/.local/bin/ukvm remote io` on Spectrum, and verify it connects
without touching io. Repeat after a UKVM service restart, logout/login and reboot
on io. A logged-in supported Wayland session is still required; no display-manager
or headless access is added. Journal diagnostics report restore-attempt/token
presence only and cannot establish that a picker was skipped.

Check colored terminal text, small fonts, icons and scrolling in the new image.
Compare in a larger/maximized window. Encoded frames remain at most 1920×1080, so
a 4K monitor is still reduced and cannot have native 4K text fidelity. Quality
increases can reduce video FPS on slow CPUs/LANs; input stays on its independent
transport with existing cleanup and bounded latest-frame queues.

`ukvm authorization reset` now explicitly forgets all three portal credentials,
including ScreenCast, after its existing confirmation and owned-runtime shutdown.
It leaves UKVM stopped. Do not use reset for this retest: it would discard the
already accepted input grants as well. Revoke individual screen-sharing consent
through the desktop's portal controls when available.
