# AGENTS.md — rules for working on tudi

This file is read by coding agents (Claude Code: `CLAUDE.md` is a symlink to
this file) and by human contributors. The master plan is `docs/PLAN.md`.

## Workflow

- Work one milestone of `docs/PLAN.md` at a time. Read the milestone's tasks
  and acceptance criteria before starting.
- Stop when the acceptance criteria pass, tick the box in PLAN.md §12, and
  report the measured numbers.
- If a criterion cannot be met, stop and report. Never weaken a criterion or a
  performance budget to make it pass.
- Record every non-obvious decision in `docs/decisions.md`
  (ADR style: context / decision / consequences).
- Keep the design docs in `docs/design/` in sync with the code you change.

## Language

- Everything outside `private/` is **English**: code, identifiers, comments,
  docs, commit messages, test names, log messages.
- User-visible UI strings are English in code, wrapped for gettext; Chinese
  goes in `po/zh_CN.po`.
- `private/` is a local, git-ignored directory for the maintainer's private
  notes (may be Chinese). Never copy its content into public files, and never
  commit anything from it.
- Test fixtures may contain CJK file names (that is a feature under test).

## Performance hard rules

These protect the budgets in PLAN.md §2. Violations are bugs.

1. No filesystem syscalls (`stat`, `statx`, `open`, `read_dir`, …) on the GTK
   main thread.
2. No O(n) loops over more than ~5,000 items per frame on the main thread.
   Batch work through worker threads and apply results with a per-frame time
   budget (~4 ms).
3. Do not use `GtkTreeListModel`, `GtkSortListModel`, `GtkFilterListModel`,
   `GtkDirectoryList`, `GtkStringList`, or `GtkTreeView` for file rows.
   The file model is our own `gio::ListModel` over the Rust row array.
4. Never iterate a `GListModel` item-by-item to implement select-all, counts,
   or similar; use the Rust-side data.
5. Never read file contents to determine icons or types while listing.
6. File names are bytes (`OsStr`/`OsString`). Convert lossily only for display.
7. Every change that could affect a budgeted metric must be followed by running
   the relevant benchmark; a >20% regression must be fixed or reported.

## Code conventions

- Rust stable (see `rust-toolchain.toml`). `cargo fmt`, `cargo clippy
  --workspace --all-targets -- -D warnings`, and `cargo test --workspace`
  must pass before every commit.
- `tudi-core`, `tudi-index`, and `tudi-proto` must not depend on GTK.
- Errors: `thiserror` in libraries, `anyhow` only in binaries.
- Logging: `tracing`. No `println!` outside the CLI output path.
- `unsafe` only in thin syscall wrappers, each block with a `// SAFETY:`
  comment. `tudi-fanotifyd` keeps dependencies minimal and has no async
  runtime.
- Prefer small modules with unit tests next to the code; property tests
  (`proptest`) for tree splicing and index mutation.
- When adding a dependency, check its latest version and license; it must pass
  `cargo deny check`.

## Commits

- Conventional, descriptive English commit messages
  (`area: what changed and why`), e.g. `core/listing: batch statx on worker pool`.
- One logical change per commit. Do not commit generated fixtures, snapshots,
  or benchmark output other than `docs/benchmarks.md`.

## Testing locally

```sh
cargo test --workspace
cargo bench -p tudi-core
# fixtures live on a real disk (ext4), not tmpfs /tmp, so numbers are realistic
python3 benches/fixtures/make_tree.py --all ~/.cache/tudi-fixtures
cargo run -p tudi-gui -- ~/.cache/tudi-fixtures/flat-100k
# root-only fanotify tests: build as yourself, run only the test binary as
# root (running cargo under sudo leaves root-owned files in target/):
cargo test -p tudi-fanotifyd --no-run   # prints the test binary path
sudo target/debug/deps/tudi_fanotifyd-<hash> --ignored
```

## Things not to do

- Do not add features outside the current milestone without asking.
- Do not add telemetry or any network access. tudi is fully local.
- Do not change performance budgets, the language rule, or the license.
