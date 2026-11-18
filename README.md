# tudi

> **Tudi (土地)** is the Chinese earth god who knows every inch of his local
> land. **tudi** knows every file on yours.

A fast, two-pane (folder tree + details list) file browser for Linux
(GTK4/libadwaita), with a
memory-driven search — type the fragments you remember, in any order — and
keyboard-reachable bookmarks.

**Status:** pre-alpha. Nothing usable yet; see `docs/PLAN.md` for the plan and
progress.

## Building

Requirements: Rust (the toolchain is pinned in `rust-toolchain.toml` and
installed by rustup), GTK ≥ 4.14 and libadwaita ≥ 1.5 development files.

```sh
# Debian/Ubuntu
sudo apt install libgtk-4-dev libadwaita-1-dev
cargo build --workspace
cargo run -p tudi-gui
```

### Install for your user

```sh
scripts/install-user.sh --pin     # ~/.local/bin, menu entry, icon; --pin adds it to the GNOME dock
scripts/install-user.sh --uninstall
```

### Usage

```sh
tudi                 # open a window (last folder), or raise the running one
tudi ~/projects      # open a folder (in the running instance, if any)
tudi --new-window    # another window
tudi --background    # start the resident instance without a window (e.g. at login)
tudi --quit          # stop the resident instance
```

tudi stays resident after its last window closes, so the next window opens
in a few milliseconds.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
