# Linux release packages

`scripts/build-linux-packages.sh` builds x86_64 `.deb`, `.rpm`, and `.tar.gz`
artifacts, plus `SHA256SUMS`. The GitHub workflow builds on Ubuntu 24.04 to
provide a glibc 2.39 baseline, validates the workspace, smoke-tests Debian
installation on Ubuntu and Fedora, and publishes downloads as a **prerelease**
on a `vVERSION` tag.
The tag must match the workspace version in `Cargo.toml`. Manual workflow runs
produce downloadable Actions artifacts without creating a release. Review the
changes before pushing a release tag. Package files belong in release
assets, not in Git source history.

## Installing downloads

Download your package from [GitHub Releases](https://github.com/gibranlp/UKVM/releases)
and verify it against the accompanying `SHA256SUMS`:

```sh
sha256sum --ignore-missing -c SHA256SUMS
# Debian / Ubuntu / compatible derivatives:
sudo apt install ./ukvm_0.1.0-1_amd64.deb
# Fedora / compatible RPM distributions:
sudo dnf install ./ukvm-0.1.0-1.x86_64.rpm
```

Use the actual downloaded version. The tarball supports other compatible Linux
distributions (including Arch), but does not resolve dependencies:

```sh
tar -xf ukvm-0.1.0-linux-x86_64.tar.gz
./ukvm-0.1.0-linux-x86_64/ukvm install
export PATH="$HOME/.local/bin:$PATH"
```

Runtime libraries include glibc >=2.39, libgcc, PipeWire, Wayland and xkbcommon.
The `.deb` and `.rpm` derive ELF dependency versions from the actual binary.
Wayland/xkbcommon are explicitly required because the viewer loads them at runtime.
Install your desktop's portal backend separately; `xdg-desktop-portal` alone does
not implement InputCapture, RemoteDesktop or ScreenCast. See
[platform support](PLATFORM_SUPPORT.md) and [remote sessions](REMOTE_SESSIONS.md).
Packaging does not make unsupported compositors operational. Older glibc systems,
32-bit Linux, ARM, Windows and macOS are outside this initial release matrix.
Package install/help/removal smoke tests pass in clean Ubuntu 24.04 and Fedora 42
containers. Both default and bundled-codec workspace checks pass (304 tests,
six opt-in tests ignored). Physical desktop/backend behavior on other distributions
still requires acceptance; the package format alone does not establish support.

The locked `turbojpeg-sys` codec source is statically built with SIMD support
for these releases. This avoids the libjpeg-turbo >=3 runtime prerequisite of
normal source builds on distributions shipping version 2. Its license notice is
included. Codec security fixes require rebuilding/releasing UKVM. Default source
builds still use the system codec; no input, transport or consent behavior changes.
This software is based in part on the work of the Independent JPEG Group.

Packages install `/usr/bin/ukvm`, `/usr/lib/systemd/user/ukvm.service` and docs.
There are no install/uninstall scriptlets: no automatic service startup, trust,
permission grants, portal prompts, or deletion of user state. As your normal user
in a Wayland graphical terminal:

```sh
/usr/bin/ukvm service install
# After setup/pairing and explicit permission choices:
/usr/bin/ukvm service enable
/usr/bin/ukvm service start
```

An existing `~/.local/bin/ukvm` may take precedence in PATH; use `/usr/bin/ukvm`
when migrating to packages. `service install` regenerates the managed unit with
that executable path. Upgrading replaces package assets; explicitly restart the
user service when ready. Before removing the package, explicitly stop/disable
the service. Identity, trust, configuration and remembered portal consent remain
in user storage. See [service lifecycle](SERVICE.md).

## Building and releasing

On Ubuntu 24.04, with stable Rust installed:

```sh
sudo apt install build-essential clang libclang-dev pkg-config \
  libpipewire-0.3-dev libwayland-dev libxkbcommon-dev cmake nasm \
  rpm dpkg-dev ripgrep
bash scripts/build-linux-packages.sh
```

Run the checks from `AGENTS.md` before releasing. Bundled builds can be validated
using `TURBOJPEG_SOURCE=vendor TURBOJPEG_STATIC=1` and Cargo's
`--features ukvm/bundled-jpeg` with workspace check/test/clippy/build commands.
The builder stages only installed executable/unit/docs and license notices; it
never includes local configuration or private state. It does not operate systemd.
Use a clean checkout for published builds. A binary built on a newer distro may
require newer glibc; the tarball does not hide that requirement.

After updating the workspace version, commit and push a matching tag (for example
`v0.1.0`). The workflow publishes a prerelease containing the three packages and
checksums through GitHub Releases. This is direct download
distribution, not an apt/dnf repository, and provides no automatic update channel
or package signing yet.
