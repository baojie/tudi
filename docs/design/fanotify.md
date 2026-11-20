# fanotify helper and security model (M7)

Code: `crates/tudi-fanotifyd` (helper and protocol),
`crates/tudi-indexd/src/helper.rs` (client). Units:
`data/tudi-fanotifyd.{socket,service}`. Plan: `docs/PLAN.md` §5.6.
Decisions: ADR-002, ADR-028, ADR-029.

## Why

inotify needs one watch per folder, so the user daemon watches only the
200 most used folders and otherwise relies on a reconcile every 10 minutes.
fanotify with `FAN_MARK_FILESYSTEM` sees every create, delete and rename
on a whole filesystem at once, but needs `CAP_SYS_ADMIN`. The helper is the
small root part that turns those events into "folder X changed" messages
for each user's daemon. It is optional: without it tudi works as before.

## Helper

One thread, one `poll` loop, `libc` as the only dependency.

1. **Setup (root).** `fanotify_init(FAN_CLASS_NOTIF | FAN_CLOEXEC |
   FAN_NONBLOCK | FAN_REPORT_DFID_NAME)`. For each filesystem (by fsid)
   mounted with a type in `mounts::SUPPORTED` (ext2/3/4, xfs, btrfs, f2fs,
   bcachefs), one `FAN_MARK_FILESYSTEM` mark for `FAN_CREATE | FAN_DELETE |
   FAN_MOVED_FROM | FAN_MOVED_TO | FAN_ONDIR`, and a folder fd of the mount
   used to resolve handles (the mount of the filesystem's own root when
   there is one). Marks that fail (e.g. a btrfs subvolume with its own
   fsid) are logged and skipped; clients see that from `Covered`.
   `FAN_DELETE_SELF` / `FAN_MOVE_SELF` are not needed: the parent folder
   gets an event for the same change. `FAN_MODIFY` / `FAN_ATTRIB` are not
   used (sizes and mtimes refresh when a folder is listed again).
2. **Socket.** From systemd (`tudi-fanotifyd.socket`,
   `/run/tudi/fanotify.sock`, mode 0666) or bound by the helper
   (`--socket`, for tests).
3. **Drop privileges** (`privs.rs`, below), then serve.
4. **Events.** Each event names the folder that changed by its file handle.
   Handles are collected in a set (deduplicated) for a batch window of
   200 ms after the first event; then each is resolved with
   `open_by_handle_at(mount_fd, handle, O_PATH)` and
   `readlink(/proc/self/fd/N)`. Handles of folders deleted since are
   skipped (the parent has its own event). If the filesystem has more than
   one mount (bind mounts), the path is translated to every mount of it.
5. **Delivery.** For each subscribed client, the folders it may see
   (below) are sent as `Changed` frames (≤ 1 MiB each).
6. **Overflow.** `FAN_Q_OVERFLOW` from the kernel (default queue: 16,384
   events), or more than 50,000 distinct folders pending in one batch:
   every client gets `Overflow` and reconciles.

## Protocol (`proto.rs`)

Frames: little-endian `u32` body length, body; the body's first byte is the
tag. Hand-written encoding, bounded sizes, decoding never panics (proptest).

| Direction | Message | Content |
|---|---|---|
| client → helper | `Subscribe` (once, within 5 s) | version, ≤ 64 absolute roots |
| helper → client | `Covered` (once) | fsids of the marked filesystems |
| helper → client | `Changed` | folder paths (bytes) |
| helper → client | `Overflow` | — |
| helper → client | `Error` | reason; then the helper closes |

Nothing else is ever accepted from a client: any further byte, an
unknown version, a malformed frame, or a missing subscription after 5 s
disconnects it. A client that does not read is disconnected once 16 MiB
are queued for it (it reconciles when it reconnects).

## Security model

**Threat.** The helper runs as root and sees every create, delete and
rename on the system. A local user must not learn, through it, any name
they could not list themselves, and must not be able to make it do
anything but send them such names.

**Who may connect.** Peer credentials come from `SO_PEERCRED` and
`SO_PEERGROUPS` (taken by the kernel at `connect()`, not claimed by the
client). Only uids 1000–60000 (regular users, Debian/Fedora `UID_MIN` …
`UID_MAX`) are accepted; root and system users are refused. At most 64
clients.

**What a client hears.** A folder `D` is sent to a client only if `D` is
one of its roots `R` or below it, the client may search (`x`) every folder
above `R`, and may list (`r` and `x`) every folder from `R` down to `D`
inclusive. So a path is revealed only if the client could have found it by
listing from its own root, which it named itself. Naming `/` or someone
else's folder as a root does not help: an unreadable folder on the way
hides everything below it. Examples are unit tests in `access.rs` and root
tests in `tests/root.rs`.

Permissions are evaluated in the helper from `lstat` (owner, group, mode)
and the folder's POSIX access ACL (`system.posix_acl_access`), following the
kernel's `posix_acl_permission`, with the client's uid, gid and
supplementary groups. A malformed or oversized ACL denies. Linux security
modules (SELinux, AppArmor) are not consulted: a client may hear about a
folder an LSM would hide from it, never one that file permissions hide.
Evaluating in user space instead of switching to the client's credentials
means the helper never needs `CAP_SETUID`.

**Subscribe-only.** The only client input is the list of roots, used as
path prefixes for filtering. The helper never opens, reads, stats or marks
a client-supplied path; it only stats folders it got from the kernel's
events, to decide visibility. Marks are set once at startup for all
supported filesystems, so clients cannot influence them either.

**Privileges** (`privs.rs`). After setup the helper

1. drops every capability except `CAP_DAC_READ_SEARCH` from its bounding
   set,
2. switches to `nobody` (no supplementary groups) keeping capabilities
   across the switch, then clears `PR_SET_KEEPCAPS`,
3. sets permitted = effective = {`CAP_DAC_READ_SEARCH`}, inheritable and
   ambient empty,
4. sets `PR_SET_NO_NEW_PRIVS`,
5. reads `/proc/self/status` and exits if any uid is 0 or any other
   capability is left.

`CAP_DAC_READ_SEARCH` is needed for `open_by_handle_at` and to `lstat`
folders the helper's own user cannot reach. It also allows reading any
file, so a compromised helper could read files; it can no longer write as
root, load modules, change marks or gain capabilities (no-new-privs, empty
bounding set). The process is not dumpable after the uid change.

**systemd hardening** (`tudi-fanotifyd.service`): capability bounding set
limited to what setup needs, `NoNewPrivileges`, `PrivateNetwork`,
`RestrictAddressFamilies=AF_UNIX`, `ProtectSystem=strict`,
`ProtectHome=read-only`, `PrivateTmp`, `PrivateDevices`, kernel
tunables/modules/logs/cgroups protected, `RestrictNamespaces`,
`MemoryDenyWriteExecute`, `SystemCallArchitectures=native`.

**Resource limits.** Client frames ≤ 64 KiB, ≤ 64 roots, paths ≤ 4096
bytes; ≤ 64 clients; ≤ 16 MiB queued per client; ≤ 50,000 pending folders
per batch; the kernel queue is the default bounded one (no
`FAN_UNLIMITED_QUEUE`).

**Code.** `unsafe` only in `sys.rs` and `privs.rs`, each block with a
`SAFETY` comment. No async runtime, no parsing crates.

## Security review checklist

Reviewed for M7 (2026-11-20).

- [x] Only regular users can subscribe; uid/gid/groups come from the
      kernel (`SO_PEERCRED`, `SO_PEERGROUPS`), never from the client.
      Test: `refuses_root_and_system_users`.
- [x] A client never receives a path outside its roots, nor one below a
      folder it cannot list, including when it names `/` or another
      user's folder as a root. Tests: `access::tests::visibility_rules`,
      `users_only_hear_about_folders_they_can_list` (two users plus a
      third naming the other's home).
- [x] POSIX ACLs are honoured (named users, groups, mask; a matching group
      without access denies instead of falling through to "other").
      Tests: `acl_*`.
- [x] Subscribe-only: the client's only input is its roots; no request
      makes the helper open, read, stat or mark a client-supplied path.
      Any extra input disconnects. Test:
      `drops_misbehaving_clients_and_keeps_serving`.
- [x] Malformed input cannot crash or stall the helper: bounded frames,
      non-blocking sockets in one `poll` loop, `decode` fuzzed with
      proptest, subscription timeout.
- [x] Resource use is bounded (frames, clients, per-client queue, pending
      folders, kernel queue); a slow client is disconnected, not waited
      for.
- [x] Privileges are dropped after setup to `nobody` with only
      `CAP_DAC_READ_SEARCH`, no-new-privs, empty ambient set, and verified
      at runtime. Test: `privileges_are_dropped_after_setup`.
- [x] Lost events are signalled (`Overflow`) and lead to a reconcile.
      Test: `queue_overflow_tells_clients_to_reconcile`.
- [x] The helper does not follow symlinks while checking access (`lstat`;
      resolved paths come from the kernel and contain no symlinks); a
      non-folder on the way denies.
- [x] Minimal code and dependencies (`libc`); `unsafe` confined to thin
      wrappers with `SAFETY` comments; systemd sandboxing on top.
- [x] Packaged separately (`tudi-fanotify`), not installed by default;
      the daemon works without it.

Known limitations (accepted): LSM policies are not evaluated; names of
folders are revealed, not of files (clients re-list the folder
themselves); a compromised helper can read files (`CAP_DAC_READ_SEARCH`).

## Client (`tudi-indexd`)

- A `fanotify` thread connects to `/run/tudi/fanotify.sock`
  (`TUDI_FANOTIFY_SOCKET` overrides), subscribes with the roots whose
  filesystem supports fanotify (`fsinfo::detect`), and marks a root
  **live** when its `statfs` fsid is in `Covered` (`tudi index status`
  shows `live`). If the helper is missing it retries with a backoff up to
  a minute and logs once.
- `Changed` batches (merged with any already queued) go to the updater,
  which finds the folders under a read lock, lists them with no lock held,
  and applies the listings under the write lock (`scan::apply_dir`: new
  sub-folders are excluded by pattern or mount, or scanned). Folders
  inside excluded folders are not in the index and are skipped. New
  entries are searchable at once (the prefilter lets nodes without a
  character bag through); search data and the snapshot refresh at most
  once a minute on the `refresher` thread, reusing the typo vocabulary
  (ADR-029).
- Live roots are reconciled at most hourly (a safety net), not every
  10 minutes; inotify events for folders in live roots are ignored.
- `Overflow` → reconcile the live roots now. Reconnecting after the startup
  pass → reconcile them too (changes in between were seen by no one).
  Disconnect → back to the normal interval and inotify.

## Testing

Unit tests (no root): protocol, access rules and ACLs, event parsing,
mountinfo and bind-mount aliases, syscall wrappers.

Root tests (`tests/root.rs`, `#[ignore]`): each starts a helper on a
private socket; clients connect as test uids 59001–59003 by switching only
the connecting thread's credentials. Build as yourself and run only the
test binary under sudo:

```sh
cargo test -p tudi-fanotifyd --no-run   # prints the tests/root.rs binary
sudo target/debug/deps/root-<hash> --ignored --test-threads 1
```

Acceptance (installed helper): `benches/m7_live.py p10` and
`benches/m7_live.py clone`; numbers in `docs/benchmarks.md`.

Install from a checkout: `scripts/install-fanotify.sh`
(`--uninstall` to remove).
