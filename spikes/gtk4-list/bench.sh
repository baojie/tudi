#!/bin/sh
# M1 spike benchmarks. Prints one JSON object per run.
#
#   bench.sh startup [N]    startup in the current desktop session (N runs);
#                           also prints the externally measured launch time.
#   bench.sh reopen [N]     P1a: close the first window, keep the process,
#                           open a new window; prints reopen_ms (N runs).
#   bench.sh all [VARIANT]  scroll + insert benchmark. Runs inside a private
#                           headless gnome-shell so the window is never
#                           occluded (an occluded Wayland window gets no frame
#                           callbacks and the benchmark would stall).
set -eu
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
BIN="$ROOT/target/release/gtk4-list-spike"
cargo build -q --release -p gtk4-list-spike --manifest-path "$ROOT/Cargo.toml"

case "${1:-}" in
startup)
    for _ in $(seq 1 "${2:-7}"); do
        t0=$(date +%s%N)
        out=$("$BIN" --bench startup 2>/dev/null)
        paint=$(printf '%s' "$out" | sed -E 's/.*"first_paint_epoch_ns":([0-9]+).*/\1/')
        printf '%s {"external_ms":%s}\n' "$out" "$(( (paint - t0) / 1000000 ))"
    done
    ;;
reopen)
    for _ in $(seq 1 "${2:-7}"); do
        "$BIN" --bench reopen 2>/dev/null
    done
    ;;
all)
    variants=${2:-"listview columnview"}
    for v in $variants; do
        dbus-run-session -- sh -c '
            gnome-shell --headless --wayland --no-x11 \
                --virtual-monitor 1920x1080@60 --wayland-display tudi-bench >/dev/null 2>&1 &
            gs=$!
            for _ in $(seq 1 100); do
                [ -S "$XDG_RUNTIME_DIR/tudi-bench" ] && break
                sleep 0.1
            done
            WAYLAND_DISPLAY=tudi-bench "$0" --variant "$1" --bench all 2>/dev/null | grep "^{"
            kill $gs
        ' "$BIN" "$v"
    done
    ;;
*)
    echo "usage: $0 startup [N] | reopen [N] | all [VARIANT]" >&2
    exit 2
    ;;
esac
