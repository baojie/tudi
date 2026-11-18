#!/bin/sh
# Install tudi for the current user under ~/.local (no root needed):
#   ~/.local/bin/{tudi,tudi-gui}
#   ~/.local/share/locale/<lang>/LC_MESSAGES/tudi.mo
#   ~/.local/share/applications/io.github.baojie.tudi.desktop
#   ~/.local/share/icons/hicolor/scalable/apps/io.github.baojie.tudi.svg
#
#   scripts/install-user.sh [--pin]   # --pin: add to the GNOME dock
#   scripts/install-user.sh --uninstall
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
PREFIX="${PREFIX:-$HOME/.local}"
ID=io.github.baojie.tudi
APPS="$PREFIX/share/applications"
ICONS="$PREFIX/share/icons/hicolor/scalable/apps"

if [ "${1:-}" = "--uninstall" ]; then
    tudi --quit 2>/dev/null || true
    tudi index stop 2>/dev/null || true
    systemctl --user disable --now tudi-indexd 2>/dev/null || true
    rm -f "$HOME/.config/systemd/user/tudi-indexd.service"
    rm -f "$PREFIX/bin/tudi" "$PREFIX/bin/tudi-gui" "$PREFIX/bin/tudi-indexd" "$APPS/$ID.desktop" "$ICONS/$ID.svg"
    find "$PREFIX/share/locale" -path '*/LC_MESSAGES/tudi.mo' -delete 2>/dev/null || true
    if command -v gsettings >/dev/null; then
        favs=$(gsettings get org.gnome.shell favorite-apps)
        new=$(printf '%s' "$favs" | sed "s/, '$ID.desktop'//; s/'$ID.desktop', //; s/'$ID.desktop'//")
        [ "$favs" = "$new" ] || gsettings set org.gnome.shell favorite-apps "$new"
    fi
    echo "tudi uninstalled from $PREFIX"
    exit 0
fi

cargo build --release -p tudi-gui -p tudi-indexd --manifest-path "$ROOT/Cargo.toml"

# A running resident instance keeps the old binary; stop it first.
"$PREFIX/bin/tudi" --quit 2>/dev/null || true
"$PREFIX/bin/tudi" index stop 2>/dev/null || true

install -Dm755 "$ROOT/target/release/tudi" "$PREFIX/bin/tudi"
install -Dm755 "$ROOT/target/release/tudi-gui" "$PREFIX/bin/tudi-gui"
install -Dm755 "$ROOT/target/release/tudi-indexd" "$PREFIX/bin/tudi-indexd"
for po in "$ROOT"/po/*.po; do
    lang=$(basename "$po" .po)
    mkdir -p "$PREFIX/share/locale/$lang/LC_MESSAGES"
    msgfmt -o "$PREFIX/share/locale/$lang/LC_MESSAGES/tudi.mo" "$po"
done
install -Dm644 "$ROOT/data/icons/hicolor/scalable/apps/$ID.svg" "$ICONS/$ID.svg"
# Absolute Exec path: the session's PATH may not include ~/.local/bin.
mkdir -p "$APPS"
sed "s|^Exec=tudi|Exec=$PREFIX/bin/tudi|" "$ROOT/data/$ID.desktop" > "$APPS/$ID.desktop"
update-desktop-database "$APPS" 2>/dev/null || true
# systemd user unit for the search index (enable it in Settings, or with
# `systemctl --user enable --now tudi-indexd`).
UNITS="$HOME/.config/systemd/user"
mkdir -p "$UNITS"
sed "s|^ExecStart=/usr/bin/|ExecStart=$PREFIX/bin/|" "$ROOT/data/tudi-indexd.service" > "$UNITS/tudi-indexd.service"
systemctl --user daemon-reload 2>/dev/null || true
gtk-update-icon-cache -q -t "$PREFIX/share/icons/hicolor" 2>/dev/null || true

if [ "${1:-}" = "--pin" ] && command -v gsettings >/dev/null; then
    favs=$(gsettings get org.gnome.shell favorite-apps)
    case "$favs" in
        *"'$ID.desktop'"*) ;;
        "@as []") gsettings set org.gnome.shell favorite-apps "['$ID.desktop']" ;;
        *) gsettings set org.gnome.shell favorite-apps "$(printf '%s' "$favs" | sed "s/]\$/, '$ID.desktop']/")" ;;
    esac
    echo "pinned to the dock"
fi
echo "tudi installed in $PREFIX/bin"
