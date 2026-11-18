# tudi

> **Tudi (土地)** is the Chinese earth god who knows every inch of his local
> land. **tudi** knows every file on yours.

A fast, two-pane (folder tree + details list) file browser for Linux
(GTK4/libadwaita), with a
memory-driven search — type the fragments you remember, in any order — and
keyboard-reachable bookmarks.

**Status:** 0.1.0, a first preview: browsing, live updates and folder
history work; search, bookmarks and launchers are planned (see
`docs/PLAN.md` and `CHANGELOG.md`).

## Building

Requirements: Rust (the toolchain is pinned in `rust-toolchain.toml` and
installed by rustup), GTK ≥ 4.14 and libadwaita ≥ 1.5 development files.

```sh
# Debian/Ubuntu
sudo apt install libgtk-4-dev libadwaita-1-dev
cargo build --workspace
cargo run -p tudi-gui
```

### Download

Packages for Ubuntu 24.04 or newer (and other distributions with GTK ≥ 4.14
and libadwaita ≥ 1.5) are on the Releases page:

```sh
# Debian / Ubuntu: installs the dependencies too
sudo apt install ./tudi_0.1.0-1_amd64.deb

# any distribution: unpack into ~/.local (needs ~/.local/bin in PATH)
tar -xzf tudi-0.1.0-x86_64-linux.tar.gz -C ~/.local --strip-components=1
```

### Install from source for your user

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
tudi q 合同 .pdf 上月  # search the index from the shell (docs/query-syntax.md)
```

Shell integration: `eval "$(tudi init bash)"` (or `zsh`, or
`tudi init fish | source`) adds `tj FRAGMENTS…` to jump to the best
matching folder; add `--hook` to count every `cd` in the folder history.

Settings live in `~/.config/tudi/config.toml` (written with all defaults
on first run): UI language (`[ui] language = "en"` or `"system"`), sorting,
hidden files, terminal and editor commands, history exclusions.

tudi stays resident after its last window closes, so the next window opens
in a few milliseconds.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
