#!/bin/sh
# Install the optional fanotify helper system-wide (needs root):
#   /usr/local/libexec/tudi-fanotifyd
#   /etc/systemd/system/tudi-fanotifyd.{socket,service}
#
#   scripts/install-fanotify.sh               # build as you, then sudo
#   scripts/install-fanotify.sh --uninstall
#
# The build runs as the calling user; only the install steps use sudo.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
BIN=/usr/local/libexec/tudi-fanotifyd
UNITS=/etc/systemd/system

if [ "${1:-}" = "--uninstall" ]; then
    sudo systemctl disable --now tudi-fanotifyd.socket tudi-fanotifyd.service 2>/dev/null || true
    sudo rm -f "$BIN" "$UNITS/tudi-fanotifyd.socket" "$UNITS/tudi-fanotifyd.service"
    sudo systemctl daemon-reload
    echo "tudi-fanotifyd uninstalled"
    exit 0
fi

cargo build --release -p tudi-fanotifyd --manifest-path "$ROOT/Cargo.toml"
sudo install -Dm755 "$ROOT/target/release/tudi-fanotifyd" "$BIN"
sudo install -Dm644 "$ROOT/data/tudi-fanotifyd.socket" "$UNITS/tudi-fanotifyd.socket"
sed "s|^ExecStart=.*|ExecStart=$BIN|" "$ROOT/data/tudi-fanotifyd.service" \
    | sudo tee "$UNITS/tudi-fanotifyd.service" >/dev/null
sudo systemctl daemon-reload
sudo systemctl enable --now tudi-fanotifyd.socket
# A running helper keeps the old binary.
sudo systemctl try-restart tudi-fanotifyd.service
echo "tudi-fanotifyd installed; tudi-indexd connects within a minute"
echo "(or restart it: tudi index stop; check: tudi index status)"
