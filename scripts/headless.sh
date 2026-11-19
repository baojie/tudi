#!/bin/sh
# Run tudi inside a private headless gnome-shell (its own D-Bus session and
# Wayland display), so GUI checks never touch the desktop session.
#
#   scripts/headless.sh [--release] [TUDI ARGS...]
#
# Environment passes through, e.g. TUDI_DEV_SCRIPT, TUDI_DEV_SCREENSHOT,
# XDG_CONFIG_HOME, XDG_STATE_HOME, LANG. TUDI_BIN runs another tudi-gui binary
# (e.g. an older release, for screenshots). Set TUDI_HEADLESS_TIMEOUT (s) to
# change the 60 s limit.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
BIN="${TUDI_BIN:-$ROOT/target/debug/tudi-gui}"
if [ "${1:-}" = "--release" ]; then BIN="$ROOT/target/release/tudi-gui"; shift; fi
exec dbus-run-session -- sh -c '
  gnome-shell --headless --wayland --no-x11 --virtual-monitor 1280x800@60 \
      --wayland-display tudi-headless >/dev/null 2>&1 &
  gs=$!
  for _ in $(seq 1 100); do
      [ -S "$XDG_RUNTIME_DIR/tudi-headless" ] && break
      sleep 0.1
  done
  WAYLAND_DISPLAY=tudi-headless timeout "${TUDI_HEADLESS_TIMEOUT:-60}" "$0" "$@" 2>&1 \
      | grep -av "Gtk-WARNING\|dbus-daemon\|GVFS\|evolution\|libedbus\|localsearch" || true
  kill $gs
' "$BIN" "$@" 2>/dev/null
