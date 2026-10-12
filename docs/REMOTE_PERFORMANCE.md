# Remote session performance comparison

The user confirmed remote viewing/control, remembered ScreenCast authorization
across reconnect/reboot, and return to physical topology on close. This follow-up
preserves that architecture. Its measurements below are local synthetic CPU
benchmarks, **not io timings or evidence of physical improvement**. The actual
capture/network/compositor pipeline requires the two-PC comparison below.

## Profile findings and implementation

Before replacing the encoder, a release-mode synthetic benchmark measured about
80 ms encoding at 1080p and 134 ms for a 4K source reduced to 1080p. Pure Rust JPEG
encoding, packed-to-RGB conversion and reduction were a clear CPU bottleneck.
Inspection also found separate 33 ms capture/encoder/send/render polls, a
post-acknowledgement bandwidth sleep that counted transmission time twice, and
JPEG decode plus bilinear scaling inside the GUI input callback thread.

The optimized path keeps independent JPEG frames, using system
[libjpeg-turbo](https://libjpeg-turbo.org/) through the maintained
[turbojpeg Rust API](https://docs.rs/turbojpeg/1.5.1/turbojpeg/). For io's
1366×768 Ethernet setup, CPU SIMD JPEG offers a practical improvement without
adding H.264 encoder latency, reordering, device negotiation or GPU dependencies.
Native geometry and 4:4:4 chroma remain intact; quality defaults to 97 instead of
92. Future negotiated H.264/hardware encoding could reduce bandwidth at higher
resolutions; it is not implemented here.

Capture drains available PipeWire buffers to the newest with bounded callback
work, immediately recycling superseded buffers through the sanctioned stream
API. Capture publishes into a single replaceable raw slot and wakes the encoder.
Packed BGRX/RGBX goes straight into the encoder; encoded bytes use shared ownership
instead of deep frame clones. The viewer decodes directly into its packed pixel
format, then scales on a dedicated worker. Native scaling shares pixels without
interpolation or an extra image copy. Prepared frames and GUI wakeups are both
coalesced. GUI callbacks handle input and submit prepared pixels; they do not
wait for JPEG decoding or scaling. Display framing also runs off the input
executor. Existing input transport, focus leases and RemoteDesktop/EIS are reused.

Defaults: native pixels, up to 60 FPS, quality 97, 500 Mbps **JPEG payload** ceiling.
These are ceilings, not promised throughput or constant allocated bandwidth.
`--fps 1..60`, `--quality 45..100`, `--bitrate 10..1000` configure video only.
The source tries lower quantization quality if a frame exceeds 8 MiB, never
reducing its geometry/chroma to conceal that bound. There is one video frame in
flight and one replaceable frame at each pending stage. Pacing starts when a send
starts, rather than after it finishes. Slow exchanges reduce sent FPS and leave
about 10% headroom; superseded frames drop. Linux video sockets have a 128 KiB
requested send buffer, 32 KiB not-sent threshold and best-effort CS1 marking.
Host/router QoS still determines actual wire priority; input sockets are unchanged.

## Local measurements

Reproduce on either computer without opening/capturing a desktop:

```sh
cargo test --release -p ukvm-platform profile_desktop_codec -- --ignored --nocapture
```

The deterministic synthetic desktop contains thin colored glyph-like strokes and
panel boundaries. Each row averages 12 frames. Copying the synthetic raw source
before the timer is excluded, just as the capture copy has its own live metric.
Scaling is measured to a 1280×720 content viewport for both paths. No window or
network is involved, so the timings must not be reported as end-to-end latency.

| Source | Path | Encode ms | Decode ms | Scale ms | JPEG bytes |
| --- | --- | ---: | ---: | ---: | ---: |
| 1366×768 | Baseline | 40.91 | 6.37 | 10.61 | 463,082 |
| 1366×768 | SIMD native | 5.34 | 4.64 | 10.23 | 649,202 |
| 1920×1080 | Baseline | 81.12 | 12.10 | 10.25 | 911,796 |
| 1920×1080 | SIMD native | 9.42 | 9.38 | 10.21 | 1,277,546 |
| 3840×2160 | Baseline, reduced to 1920×1080 | 134.78 | 16.27 | 10.25 | 1,324,239 |
| 3840×2160 | SIMD native, all pixels retained | 33.89 | 38.99 | 10.37 | 5,108,306 |

These measurements were made on Spectrum (AMD Ryzen 7 3700X), using the installed
stable Rust toolchain. These numbers vary with CPU load/frequency and image content. The larger 4K
decode cost is expected because the new path keeps four times as many pixels.
Native-size presentation avoids the measured scaling pass entirely. io was
already below 1080p, so its original image was not being reduced at capture;
higher quantization quality and avoiding local rescaling address its quality.
The initial native-size window request may be overridden by a tiling compositor.

Both codecs remain CPU paths. Source capture needs one validated memory copy
before returning the PipeWire buffer. The GUI still copies prepared rows into a
softbuffer and the compositor presents it. There is no UKVM GPU encode/decode
path or claim of GPU profiling. At 60 FPS, a 5.34 ms encoding stage would consume
roughly 32% of one CPU core, before capture/transport overhead; this is a budget
estimate, not measured io CPU use. High-detail 4K JPEG cannot promise 60 FPS on
this CPU/link, and LAN bandwidth can become its primary limit.

Validation passed formatting, workspace check, the full workspace test suite
(300 passed, zero failures, six opt-in tests ignored), Clippy with warnings denied,
and debug/release workspace builds. The ignored release codec benchmark above
was also run explicitly and passed. Tests cover preference/geometry bounds,
clock-offset uncertainty and malformed exchanges, bounded statistics and newest
queues, PipeWire stale-buffer draining, native chroma/detail, shared native pixels,
socket backlog settings and existing permission/revocation/input cleanup paths.

## Live diagnostics

```sh
~/.local/bin/ukvm remote io --stats
```

Every five seconds and at session shutdown, stderr reports numeric metadata only:

- Capture FPS; receive and unique submitted-image FPS; JPEG payload Mbps.
- Source, encoded and rendered content resolution; mode and configured ceilings.
- Capture copy, source encoder queue, encode, sender queue, viewer decoder queue,
  decode, scale, prepared-frame queue and surface submission times.
- PipeWire stale-buffer drops and intentional FPS-throttle skips; raw, encoded,
  decode and prepared-frame overwrite counts and sampled queue
  depths (each pending queue is zero or one).
- Previous write/ack exchange time; estimated transit and frame age, with clock
  uncertainty; frame-age mean/p95 and a clock-independent lower-bound age.

FPS/bitrate are session averages from the first received frame. Viewer stage
means/p95 use the most recent 300 submitted images. Capture and source queue
timings are sampled from the latest received source metadata; encode mean uses
submitted samples. Queue depths are snapshots, not high-water marks. Image
repeats for damage-only liveness count in receive/submit rates and have old ages;
use continuous scrolling/window motion when comparing latency.

The four-timestamp ack estimates viewer-minus-source monotonic clock offset.
Half the exchange delay gives uncertainty from path asymmetry. No synchronized
wall clocks are assumed. `transit_est_ms` covers source framing/write through
viewer message acceptance, including transport scheduling; it is not a pure
wire propagation measurement. The first frame has no clock estimate. Negative
small transit estimates can occur when the next exchange differs from the clock
sample; assess them alongside uncertainty. `age_est_ms` starts when a mapped
PipeWire buffer is accepted and ends after softbuffer submission. It excludes
capture buffering before that boundary and compositor/display scan-out.
`age_lower_ms` excludes transit altogether and does not assume clock alignment.
`submit_ms` measures buffer preparation/submission, not confirmed display
presentation. `scanout=unmeasured` is deliberate: there is no presentation-time
feedback or photon measurement in this pass. No pixels, keys, input positions,
clipboard data or portal credentials are logged.

## Two-PC comparison

Spectrum is redeployed after validation under the user's standing authorization.
On **io**, after the commit has been published, from its graphical terminal:

```sh
cd ~/UKVM
git pull --ff-only
sudo pacman -S --needed libjpeg-turbo
pkg-config --atleast-version=3.0 libturbojpeg
cargo build --release --locked --workspace
./target/release/ukvm install
~/.local/bin/ukvm service install
~/.local/bin/ukvm service restart
~/.local/bin/ukvm remote --help
~/.local/bin/ukvm status --json
```

Both managed runtimes must report `display_protocol_version: 2` in their status
snapshots; Input Protocol stays 1.4. There is no identity, pairing, permission,
portal or service architecture migration. Keep the existing grants; do not reset
authorization or reinstall the portal backend. A Display 1 binary fails closed.

On **Spectrum**, run these **sequentially**, closing the first window before the
second. `--baseline` fixes the previous codec/1080p cap/30 FPS/quality/pacing and
GUI decode path within Display 2; it is an emulation, not the previous executable.

```sh
~/.local/bin/ukvm remote io --baseline --stats 2>&1 | tee /tmp/ukvm-remote-baseline.log
~/.local/bin/ukvm remote io --stats 2>&1 | tee /tmp/ukvm-remote-native.log
```

Use the same io desktop/application and comparable window size. For at least 30
seconds each, compare small colored text, drag a window and continuously scroll.
Click to control, exercise keys/modifiers/buttons/scroll, release with
Ctrl+Alt+Shift+Escape, resume, then close. Compare age estimates/p95 and uncertainty,
decode/scale/submit times, effective FPS, queue drops and bitrate. Verify normal
edge control, clipboard and file transfer afterward. A native-sized content
viewport gives the fairest text-quality check; a compositor-forced resized window
will interpolate the original 1366×768 pixels.

For a slower Ethernet link or competing traffic, compare a lower video ceiling:

```sh
~/.local/bin/ukvm remote io --stats --fps 60 --quality 97 --bitrate 80
```

This lowers video throughput/FPS while retaining native geometry. If preferred,
try `--quality 100` for text at higher bandwidth. Report the logs and your
perceptual comparison; no physical improvement is claimed until you test it.
