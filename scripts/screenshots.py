#!/usr/bin/env python3
"""Screenshots of a tudi release for docs/screenshots/ (shown in
docs/releases.md).

    scripts/screenshots.py VERSION TUDI_GUI_BINARY OUT_DIR

Builds a demo home folder (made-up names, no real files), mounts it at
/home/demo in a bubblewrap sandbox (an empty /home otherwise, so nothing of
the real user shows), runs the given tudi-gui in a private headless
gnome-shell (scripts/headless.sh) with that home and fresh config/state, drives it with TUDI_DEV_SCRIPT and renders
the window to OUT_DIR/VERSION[-N].png. Each version's script shows what
that release added. The image pixels are generated here (no Pillow).
"""

import os
import struct
import subprocess
import sys
import tempfile
import zlib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
# A fixed modification date (2026-11-03 10:24 UTC) so the shots are stable.
MTIME = 1793701440


def png(path, w, h, base, accent):
    """A simple picture: a vertical gradient with a sun and two hills."""
    rows = []
    cx, cy, r = w * 0.7, h * 0.35, h * 0.16
    for y in range(h):
        t = y / h
        row = bytearray([0])
        for x in range(w):
            if (x - cx) ** 2 + (y - cy) ** 2 < r * r:
                c = (255, 236, 160)
            elif y > h * 0.62 + 40 * ((x / w - 0.3) ** 2) * 4:
                c = tuple(int(a * (0.75 + 0.25 * (1 - t))) for a in accent)
            else:
                c = tuple(int(b * (1 - t) + 255 * t * 0.6) for b in base)
            row += bytes(c)
        rows.append(bytes(row))
    raw = zlib.compress(b"".join(rows), 6)

    def chunk(kind, data):
        c = struct.pack(">I", len(data)) + kind + data
        return c + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)

    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n")
        f.write(chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)))
        f.write(chunk(b"IDAT", raw))
        f.write(chunk(b"IEND", b""))


def demo_home(home):
    """A small, plausible home folder."""
    files = {
        "Documents/Reports/annual-report-2025.pdf": 2_400_000,
        "Documents/Reports/quarterly-review-q3.pdf": 860_000,
        "Documents/Reports/report-template.docx": 48_000,
        "Documents/budget-2026.xlsx": 132_000,
        "Documents/meeting-notes.md": 6_200,
        "Documents/reading-list.txt": 1_900,
        "Documents/project-proposal.odt": 74_000,
        "Documents/合同/租房合同.pdf": 512_000,
        "Documents/合同/劳动合同.pdf": 388_000,
        "Projects/website/index.html": 8_400,
        "Projects/website/style.css": 3_100,
        "Projects/website/app.js": 21_000,
        "Projects/website/README.md": 2_300,
        "Projects/website/package.json": 900,
        "Projects/website/logo.svg": 4_600,
        "Projects/notes-app/main.rs": 12_500,
        "Projects/notes-app/Cargo.toml": 700,
        "Projects/notes-app/README.md": 1_800,
        "Projects/data-analysis/analysis.ipynb": 340_000,
        "Projects/data-analysis/survey-results.csv": 1_250_000,
        "Music/playlist.m3u": 1_200,
        "Downloads/installer.tar.gz": 48_000_000,
        "Downloads/paper-draft.pdf": 1_700_000,
        "notes.txt": 3_400,
        "todo.md": 1_100,
    }
    for rel, size in files.items():
        p = os.path.join(home, rel)
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, "wb") as f:
            f.truncate(size)
    for d in ["Projects/website/assets", "Projects/website/src", "Music/Albums", "Videos",
              "Desktop", "Documents/Archive"]:
        os.makedirs(os.path.join(home, d), exist_ok=True)
    pics = [
        ("beach-sunset", (250, 140, 90), (60, 110, 160)),
        ("mountain-lake", (110, 170, 230), (60, 120, 80)),
        ("city-night", (40, 50, 110), (70, 70, 90)),
        ("forest-trail", (150, 200, 150), (40, 100, 50)),
        ("desert-dunes", (240, 200, 140), (200, 150, 80)),
        ("harbor-morning", (180, 210, 240), (50, 90, 130)),
        ("snow-peaks", (200, 220, 245), (230, 235, 245)),
        ("autumn-park", (240, 180, 120), (170, 90, 40)),
    ]
    os.makedirs(os.path.join(home, "Pictures"), exist_ok=True)
    for i, (name, base, accent) in enumerate(pics):
        png(os.path.join(home, "Pictures", f"{name}.png"), 320, 220, base, accent)
    for top, dirs, names in os.walk(home):
        for n in dirs + names:
            os.utime(os.path.join(top, n), (MTIME, MTIME))


# Folders on the bookmarks bar (written to bookmarks.toml, no toasts).
BOOKMARKS = ["Documents", "Projects/website", "Pictures", "Downloads"]


def scripts(version, home, out):
    """Per version: (TUDI_DEV_SCRIPT, environment changes). Each shows what
    the version added."""
    h = home
    if version == "0.1.0":
        return [(f"wait 800; nav {h}/Projects/website; wait 500; select 3; wait 600; "
                 f"shot {out}/{version}.png; quit", {})]
    if version == "0.1.1":
        # Visits for the frequent folders (bookmarks come from the config).
        bookmarks = "; ".join(f"open {h}/{d}" for d in BOOKMARKS + ["Projects", "Documents"])
        return [
            # Memory search: fragments in any order.
            (f"wait 800; {bookmarks}; nav {h}; wait 500; search x; wait 5000; search report; wait 3000; "
             f"shot {out}/{version}.png; quit", {}),
            # Bookmarks bar and Ctrl+F filtering.
            (f"wait 800; {bookmarks}; open {h}/Documents/Reports; open {h}/Projects; "
             f"nav {h}/Documents; wait 600; find re; wait 400; findenter; wait 600; "
             f"shot {out}/{version}-2.png; quit", {}),
        ]
    if version == "0.1.2":
        bookmarks = "; ".join(f"open {h}/{d}" for d in BOOKMARKS + ["Projects"])
        return [
            # Tabs, side by side, thumbnails.
            (f"wait 800; {bookmarks}; nav {h}/Projects/website; newtab {h}/Documents; "
             f"newtab {h}/Pictures; wait 400; tile; wait 600; thumbs on; wait 2500; "
             f"shot {out}/{version}.png; quit", {}),
            # One of the new languages.
            (f"wait 800; {bookmarks}; nav {h}/Documents; newtab {h}/Projects; wait 400; "
             f"tab 0; wait 600; shot {out}/{version}-2.png; quit", {"lang": "ja"}),
        ]
    raise SystemExit(f"no screenshot script for {version}")


def main():
    if len(sys.argv) != 4:
        raise SystemExit(__doc__)
    version, binary, out = sys.argv[1], os.path.abspath(sys.argv[2]), os.path.abspath(sys.argv[3])
    os.makedirs(out, exist_ok=True)
    home = "/home/demo"
    # The index daemon may still be writing its state when the run ends.
    with tempfile.TemporaryDirectory(prefix="tudi-shots-", ignore_cleanup_errors=True) as tmp:
        real_home = os.path.join(tmp, "home")
        demo_home(real_home)
        for n, (script, opts) in enumerate(scripts(version, home, out)):
            run = os.path.join(tmp, f"run{n}")
            env = dict(os.environ)
            env.update({
                "HOME": home,
                "XDG_CONFIG_HOME": os.path.join(run, "config"),
                "XDG_STATE_HOME": os.path.join(run, "state"),
                "XDG_DATA_HOME": os.path.join(run, "data"),
                "XDG_CACHE_HOME": os.path.join(run, "cache"),
                "LANG": "en_US.UTF-8",
                "TUDI_BIN": binary,
                "TUDI_DEV_SCRIPT": script,
                "TUDI_HEADLESS_TIMEOUT": "90",
            })
            # A private runtime dir: the index daemon's socket must not be
            # the user's (a running tudi would talk to the demo index).
            runtime = os.path.join(run, "runtime")
            os.makedirs(runtime, mode=0o700)
            env["XDG_RUNTIME_DIR"] = runtime
            os.makedirs(os.path.join(run, "config/tudi"))
            state = os.path.join(run, "state/tudi")
            os.makedirs(state)
            with open(os.path.join(state, "state.toml"), "w") as f:
                f.write("width = 1180\nheight = 720\ntree_width = 250\n")
            if version != "0.1.0":
                with open(os.path.join(run, "config/tudi/bookmarks.toml"), "w") as f:
                    for d in BOOKMARKS:
                        f.write(f'[[bookmark]]\npath = "{home}/{d}"\n\n')
            if "lang" in opts:
                with open(os.path.join(run, "config/tudi/config.toml"), "w") as f:
                    f.write(f'[ui]\nlanguage = "{opts["lang"]}"\n')
            # No real disks in the tree: plain Unix volume monitor, and the
            # usual mount points emptied.
            env["GIO_USE_VOLUME_MONITOR"] = "unix"
            sandbox = ["bwrap", "--dev-bind", "/", "/", "--tmpfs", "/home",
                       "--tmpfs", "/media", "--tmpfs", "/run/media", "--tmpfs", "/mnt",
                       "--bind", real_home, home, "--bind", ROOT, ROOT]
            bindir = os.path.dirname(binary)
            if bindir.startswith("/home/"):
                sandbox += ["--bind", bindir, bindir]
            subprocess.run(sandbox + [os.path.join(ROOT, "scripts/headless.sh"), home],
                           env=env, stdout=subprocess.DEVNULL, check=False)
            # The index daemon outlives the window: stop this run's.
            subprocess.run(["pkill", "-f", os.path.join(bindir, "tudi-indexd")], check=False)
    for f in sorted(os.listdir(out)):
        if f.startswith(version):
            print(os.path.join(out, f))


if __name__ == "__main__":
    main()
