#!/bin/sh
# Build release packages into dist/:
#   tudi_<version>-1_amd64.deb             (Debian/Ubuntu, pulls in GTK deps)
#   tudi-<version>-x86_64-linux.tar.gz     (prefix layout: bin/, share/)
#
#   scripts/package.sh "Name <email>"     # Debian Maintainer field
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
MAINTAINER=${1:?usage: scripts/package.sh "Name <email>"}
cd "$ROOT"
VERSION=$(cargo metadata --no-deps --format-version 1 \
    | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="tudi-gui"))')

cargo build --release -p tudi-gui -p tudi-indexd
for po in po/*.po; do
    lang=$(basename "$po" .po)
    mkdir -p "target/locale-dist/$lang/LC_MESSAGES"
    msgfmt -o "target/locale-dist/$lang/LC_MESSAGES/tudi.mo" "$po"
done

mkdir -p dist
cargo deb -p tudi-gui --no-build --maintainer "$MAINTAINER" -o dist/

STAGE="target/tudi-$VERSION-x86_64-linux"
rm -rf "$STAGE"
install -Dm755 target/release/tudi "$STAGE/bin/tudi"
install -Dm755 target/release/tudi-gui "$STAGE/bin/tudi-gui"
install -Dm755 target/release/tudi-indexd "$STAGE/bin/tudi-indexd"
install -Dm644 data/io.github.baojie.tudi.desktop "$STAGE/share/applications/io.github.baojie.tudi.desktop"
install -Dm644 data/icons/hicolor/scalable/apps/io.github.baojie.tudi.svg \
    "$STAGE/share/icons/hicolor/scalable/apps/io.github.baojie.tudi.svg"
cp -r target/locale-dist "$STAGE/share/locale"
install -Dm644 data/tudi-indexd.service "$STAGE/share/systemd/user/tudi-indexd.service"
cp README.md LICENSE-MIT LICENSE-APACHE "$STAGE/"
tar -C target -czf "dist/tudi-$VERSION-x86_64-linux.tar.gz" "tudi-$VERSION-x86_64-linux"
(cd dist && sha256sum ./*.deb ./*.tar.gz > SHA256SUMS)
ls -l dist
