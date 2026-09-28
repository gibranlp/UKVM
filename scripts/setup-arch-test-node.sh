#!/usr/bin/env bash
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: bash scripts/setup-arch-test-node.sh [--dry-run]

Set up an Arch Linux user account to build and test UKVM. Installs Arch build
packages, CMake/Ninja and Hyprland's protocol scanner for the optional portal
trial, the stable Rust toolchain, rustfmt and Clippy. In a UKVM checkout, it
also builds and runs the headless test suite.

The pacman step performs a full system upgrade to avoid an Arch partial upgrade.
It asks for sudo authentication and pacman's confirmation; no packages or
system settings are changed by --dry-run.
EOF
}

case "${1:-}" in
    --help|-h) usage; exit 0 ;;
    --dry-run) dry_run=1 ;;
    "") dry_run=0 ;;
    *) usage >&2; exit 2 ;;
esac
if (( $# > 1 )); then
    usage >&2
    exit 2
fi

if [[ "$(uname -s)" != Linux || ! -f /etc/arch-release ]]; then
    echo "This setup script supports Arch Linux only." >&2
    exit 1
fi

if (( dry_run == 0 )); then
    if (( EUID == 0 )); then
        echo "Run as your normal user; the script uses sudo only for pacman." >&2
        exit 1
    fi
    if ! command -v sudo >/dev/null 2>&1; then
        echo "sudo is required for the package installation step." >&2
        exit 1
    fi
fi

run() {
    printf '  '
    printf '%q ' "$@"
    printf '\n'
    if (( dry_run == 0 )); then
        "$@"
    fi
}

echo "UKVM Arch Linux test-node setup"
echo "Installing build tools, portal trial prerequisites, Git, CA certificates and Arch's rustup package."
echo "Pacman will show the full upgrade and request confirmation."
run sudo pacman -Syu --needed base-devel ca-certificates cmake git hyprwayland-scanner ninja pkgconf rustup

echo "Installing stable Rust, rustfmt and Clippy for this user."
run rustup toolchain install stable --profile minimal --component clippy,rustfmt
run rustup default stable

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_dir="$(cd -- "$script_dir/.." && pwd)"
if [[ ! -f "$repo_dir/Cargo.toml" && -f "$PWD/Cargo.toml" ]]; then
    repo_dir="$PWD"
fi

if [[ -f "$repo_dir/Cargo.toml" ]]; then
    echo "Building and testing UKVM in $repo_dir"
    if (( dry_run == 0 )); then
        cd -- "$repo_dir"
    fi
    run cargo --version
    run rustc --version
    run cargo build --locked --workspace
    run cargo test --locked --workspace
    if (( dry_run == 0 )); then
        echo "Done. Executable: $repo_dir/target/debug/ukvm"
    else
        echo "Dry run complete. No packages were installed or built."
    fi
else
    echo "Toolchain setup complete. Copy or clone the UKVM repository, then run:"
    echo "  cargo build --locked --workspace"
    echo "  cargo test --locked --workspace"
fi
