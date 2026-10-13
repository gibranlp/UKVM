#!/usr/bin/env bash
# Build on the oldest supported distro; do not package a rolling-distro binary.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
if [[ $# != 0 ]]; then
    echo 'Usage: bash scripts/build-linux-packages.sh (see docs/PACKAGING.md)' >&2
    exit 2
fi
case "$(uname -m)" in
    x86_64) deb_arch=amd64; rpm_arch=x86_64 ;;
    *) echo 'Release packaging currently supports Linux x86_64 only.' >&2; exit 1 ;;
esac
[[ "$(uname -s)" == Linux ]]
for tool in cargo dpkg-deb dpkg-shlibdeps rpmbuild strip readelf sha256sum rg; do
    command -v "$tool" >/dev/null
done
version=$(sed -n 's/^version = "\([0-9][0-9.]*\)"$/\1/p' Cargo.toml)
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]
if [[ -n "${RELEASE_TAG:-}" && "$RELEASE_TAG" != "v$version" ]]; then
    echo "Release tag must match Cargo.toml: v$version" >&2
    exit 1
fi
export TURBOJPEG_SOURCE=vendor TURBOJPEG_STATIC=1
cargo build --release --locked -p ukvm --features bundled-jpeg
binary="${CARGO_TARGET_DIR:-target}/release/ukvm"
# A bundled build must not accidentally acquire a system-codec dependency.
if readelf -d "$binary" | rg -q 'NEEDED.*libturbojpeg'; then
    echo 'Release binary unexpectedly depends on system libturbojpeg.' >&2
    exit 1
fi
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
stage="$work/stage"
"$binary" install --prefix /usr --destdir "$stage"
strip "$stage/usr/bin/ukvm"
install -m644 LICENSE "$stage/usr/share/doc/ukvm/copyright"
install -m644 docs/PACKAGING.md "$stage/usr/share/doc/ukvm/PACKAGING.md"
# The codec's licenses must accompany the statically linked release binary.
codec_license=$(find "${CARGO_HOME:-$HOME/.cargo}/registry/src" -path '*/turbojpeg-sys-*/libjpeg-turbo/LICENSE.md' -print -quit)
[[ -n "$codec_license" ]]
install -m644 "$codec_license" "$stage/usr/share/doc/ukvm/libjpeg-turbo-LICENSE.md"
install -m644 "$(dirname -- "$codec_license")/README.ijg" "$stage/usr/share/doc/ukvm/libjpeg-turbo-README.ijg"
mkdir -p dist "$work/debian"
cat > "$work/debian/control" <<EOF
Source: ukvm
Section: net
Priority: optional
Maintainer: UniversalKVM maintainers <root@gibranlp.dev>

Package: ukvm
Architecture: $deb_arch
Description: Software KVM and authorized interactive remote sessions for Linux Wayland
EOF
# Derive ABI requirements from the actual ELF, rather than guessing glibc versions.
deps=$(cd "$work" && dpkg-shlibdeps -O -e"$stage/usr/bin/ukvm")
deps=${deps#shlibs:Depends=}
mkdir -p "$stage/DEBIAN"
cat > "$stage/DEBIAN/control" <<EOF
Package: ukvm
Version: $version-1
Section: net
Priority: optional
Architecture: $deb_arch
Maintainer: UniversalKVM maintainers <root@gibranlp.dev>
Homepage: https://github.com/gibranlp/UKVM
Installed-Size: $(du -sk "$stage/usr" | cut -f1)
Depends: $deps, libwayland-client0, libxkbcommon0, xdg-desktop-portal
Recommends: systemd, pipewire
Description: Software KVM and authorized interactive remote sessions for Linux Wayland
 Each node can capture or receive explicitly authorized input over pinned TLS.
 Requires a compatible desktop portal backend and local user consent.
 Installation never enables or starts the user service.
EOF
dpkg-deb --root-owner-group --build "$stage" "dist/ukvm_${version}-1_${deb_arch}.deb"
rm -r -- "$stage/DEBIAN"
mkdir -p "$work/rpm/BUILD" "$work/rpm/BUILDROOT" "$work/rpm/RPMS" "$work/rpm/SOURCES" "$work/rpm/SPECS" "$work/rpm/SRPMS"
rpmbuild -bb packaging/ukvm.spec --target "$rpm_arch" \
    --define "_topdir $work/rpm" --define "ukvm_version $version" \
    --define "ukvm_stage $stage"
cp "$work/rpm/RPMS/$rpm_arch/ukvm-$version-1.$rpm_arch.rpm" dist/
archive="ukvm-$version-linux-$rpm_arch"
mkdir -p "$work/$archive"
cp "$stage/usr/bin/ukvm" "$work/$archive/"
cp -r "$stage/usr/share/doc/ukvm" "$work/$archive/docs"
tar -C "$work" -czf "dist/$archive.tar.gz" "$archive"
(
    cd dist
    sha256sum "ukvm_${version}-1_${deb_arch}.deb" "ukvm-$version-1.$rpm_arch.rpm" "$archive.tar.gz" > SHA256SUMS
)
echo 'Packages and SHA256SUMS are in dist/.'
