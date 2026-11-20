#!/bin/sh
# Install what building, running and developing tudi needs (Debian/Ubuntu).
# Only missing packages are installed; when nothing is missing, sudo is
# never called. Safe to run again.
#
#   scripts/install-deps.sh          # build + run: compilers, GTK, GStreamer, Rust
#   scripts/install-deps.sh --dev    # also tests, benchmarks, packaging, screenshots
#   scripts/install-deps.sh --list   # print the packages, install nothing
set -eu

# Build: C toolchain (rusqlite's bundled SQLite), pkg-config, GTK ≥ 4.14,
# libadwaita ≥ 1.5, GStreamer (video frames in the preview pane), gettext
# (msgfmt for translations), curl (rustup).
BUILD="build-essential pkg-config libgtk-4-dev libadwaita-1-dev
libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev gettext curl"
# Run: GStreamer decoders for common video formats (preview pane).
RUN="gstreamer1.0-plugins-base gstreamer1.0-plugins-good
gstreamer1.0-plugins-bad gstreamer1.0-libav"
# Development: git, python3 (fixtures, benchmarks, scripts), a headless
# gnome-shell with its own D-Bus session (scripts/headless.sh), bubblewrap
# (scripts/screenshots.py), GStreamer tools (test videos), GTK icon cache
# and desktop database tools (install-user.sh).
DEV="git python3 gnome-shell dbus-daemon bubblewrap gstreamer1.0-tools
libgtk-4-bin desktop-file-utils"
# Cargo tools for development (installed with cargo, as you).
CARGO_TOOLS="cargo-deny cargo-deb"

dev=false
list=false
for a in "$@"; do
    case "$a" in
        --dev) dev=true ;;
        --list) list=true ;;
        *) echo "usage: $0 [--dev] [--list]" >&2; exit 2 ;;
    esac
done

pkgs="$BUILD $RUN"
$dev && pkgs="$pkgs $DEV"
pkgs=$(echo $pkgs)

if $list; then
    echo "apt: $pkgs"
    $dev && echo "cargo install: $CARGO_TOOLS"
    exit 0
fi

if ! command -v apt-get >/dev/null || ! command -v dpkg-query >/dev/null; then
    echo "This script supports Debian/Ubuntu (apt). Install the equivalents of:" >&2
    echo "  $pkgs" >&2
    exit 1
fi

missing=""
for p in $pkgs; do
    if ! dpkg-query -W -f='${Status}' "$p" 2>/dev/null | grep -q "install ok installed"; then
        missing="$missing $p"
    fi
done
if [ -n "$missing" ]; then
    echo "installing:$missing"
    # shellcheck disable=SC2086
    sudo apt-get install -y $missing
else
    echo "system packages: all installed"
fi

# Rust: rustup installs the toolchain pinned in rust-toolchain.toml on the
# first cargo command.
if ! command -v cargo >/dev/null && [ ! -x "$HOME/.cargo/bin/cargo" ]; then
    echo "installing rustup (minimal profile)"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
fi
CARGO=$(command -v cargo || echo "$HOME/.cargo/bin/cargo")

if $dev; then
    for t in $CARGO_TOOLS; do
        if ! "$CARGO" "${t#cargo-}" --version >/dev/null 2>&1; then
            echo "installing $t"
            "$CARGO" install --locked "$t"
        fi
    done
fi
echo "dependencies ready"
