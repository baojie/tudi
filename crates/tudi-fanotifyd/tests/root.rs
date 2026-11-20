//! Integration tests for the helper. They need root (fanotify filesystem
//! marks) and are `#[ignore]`d. Build as yourself, run only the test
//! binary under sudo:
//!
//! ```sh
//! cargo test -p tudi-fanotifyd --no-run   # prints tests/root.rs's binary
//! sudo target/debug/deps/root-<hash> --ignored --test-threads 1
//! ```
//!
//! Each test starts its own helper on a private socket. Clients connect
//! as other users by switching the connecting thread's credentials (raw
//! syscalls change only the calling thread); `SO_PEERCRED` records them
//! at `connect()`. Test folders live under `/var/tmp` (a real disk, and
//! reachable by any uid), or `$TUDI_FANOTIFY_TEST_DIR`.

use std::collections::BTreeSet;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{PermissionsExt, chown};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tudi_fanotifyd::proto::{self, HelperMsg};

const ALICE: u32 = 59001;
const BOB: u32 = 59002;
const CAROL: u32 = 59003;

fn require_root() {
    // SAFETY: geteuid cannot fail.
    assert_eq!(
        unsafe { libc::geteuid() },
        0,
        "run this test binary as root (see the module docs)"
    );
}

struct Helper {
    child: Child,
    sock: PathBuf,
    base: PathBuf,
    run_dir: PathBuf,
}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.base);
        let _ = std::fs::remove_dir_all(&self.run_dir);
    }
}

fn start(name: &str) -> Helper {
    require_root();
    let id = format!("tudi-fanotify-test-{}-{name}", std::process::id());
    let base = std::env::var_os("TUDI_FANOTIFY_TEST_DIR")
        .map_or_else(|| PathBuf::from("/var/tmp"), PathBuf::from)
        .join(&id);
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o755)).unwrap();
    let run_dir = PathBuf::from("/run").join(&id);
    let sock = run_dir.join("fanotify.sock");
    let child = Command::new(env!("CARGO_BIN_EXE_tudi-fanotifyd"))
        .args(["--socket", sock.to_str().unwrap(), "--batch-ms", "50"])
        .stdin(Stdio::null())
        .spawn()
        .expect("start helper");
    let t = Instant::now();
    while UnixStream::connect(&sock).is_err() {
        assert!(
            t.elapsed() < Duration::from_secs(10),
            "helper did not start"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // The probe connection above (as root) is refused; give the helper a
    // moment to finish dropping privileges.
    std::thread::sleep(Duration::from_millis(100));
    Helper {
        child,
        sock,
        base,
        run_dir,
    }
}

/// Connect to `sock` as `uid` (gid = uid, no supplementary groups).
fn connect_as(sock: &Path, uid: u32) -> UnixStream {
    let sock = sock.to_path_buf();
    std::thread::spawn(move || {
        if uid != 0 {
            // SAFETY: raw credential syscalls with integer arguments; they
            // change only this short-lived thread (glibc's wrappers would
            // change every thread). The saved uid stays 0.
            unsafe {
                assert_eq!(
                    libc::syscall(libc::SYS_setgroups, 0, std::ptr::null::<u32>()),
                    0
                );
                assert_eq!(libc::syscall(libc::SYS_setresgid, uid, uid, -1i32), 0);
                assert_eq!(libc::syscall(libc::SYS_setresuid, uid, uid, -1i32), 0);
            }
        }
        UnixStream::connect(&sock).expect("connect")
    })
    .join()
    .unwrap()
}

fn subscribe(sock: &Path, uid: u32, roots: &[&Path]) -> UnixStream {
    let mut s = connect_as(sock, uid);
    let roots: Vec<Vec<u8>> = roots
        .iter()
        .map(|p| p.as_os_str().as_encoded_bytes().to_vec())
        .collect();
    proto::write_subscribe(&mut s, &roots).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    match proto::read_helper_msg(&mut s).unwrap() {
        HelperMsg::Covered(ids) => {
            let fs = tudi_fanotifyd::sys::fsid_path(Path::new("/var/tmp")).unwrap();
            assert!(ids.contains(&fs), "test folder's filesystem not covered");
        }
        m => panic!("expected Covered, got {m:?}"),
    }
    s
}

#[derive(Default, Debug)]
struct Got {
    dirs: BTreeSet<PathBuf>,
    overflows: usize,
    /// When each folder first arrived.
    first: std::collections::HashMap<PathBuf, Instant>,
}

/// Read messages for `dur` (or until `stop` is satisfied).
fn collect(s: &mut UnixStream, dur: Duration, stop: impl Fn(&Got) -> bool) -> Got {
    let mut g = Got::default();
    let end = Instant::now() + dur;
    loop {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() || stop(&g) {
            return g;
        }
        s.set_read_timeout(Some(left)).unwrap();
        match proto::read_helper_msg(s) {
            Ok(HelperMsg::Changed(d)) => {
                for p in d {
                    let p = PathBuf::from(std::ffi::OsString::from(String::from_utf8(p).unwrap()));
                    g.first.entry(p.clone()).or_insert_with(Instant::now);
                    g.dirs.insert(p);
                }
            }
            Ok(HelperMsg::Overflow) => g.overflows += 1,
            Ok(m) => panic!("unexpected {m:?}"),
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return g;
            }
            Err(e) => panic!("read: {e}"),
        }
    }
}

fn mkdir(p: &Path, mode: u32, owner: u32) {
    std::fs::create_dir(p).unwrap();
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
    chown(p, Some(owner), Some(owner)).unwrap();
}

fn touch(p: &Path) {
    std::fs::File::create(p).unwrap().write_all(b"x").unwrap();
}

#[test]
#[ignore = "needs root"]
fn forwards_new_folders_and_files_within_a_second() {
    let h = start("fast");
    let a = h.base.join("a");
    mkdir(&a, 0o755, ALICE);
    let mut s = subscribe(&h.sock, ALICE, &[&a]);
    let t = Instant::now();
    std::fs::create_dir_all(a.join("x/y")).unwrap();
    touch(&a.join("x/y/new.txt"));
    let want: BTreeSet<PathBuf> = [a.clone(), a.join("x"), a.join("x/y")].into();
    let g = collect(&mut s, Duration::from_secs(3), |g| want.is_subset(&g.dirs));
    assert!(want.is_subset(&g.dirs), "got {:?}", g.dirs);
    let latency = g.first[&a.join("x/y")] - t;
    eprintln!("latency: {latency:?}");
    assert!(latency < Duration::from_secs(1));
    assert!(g.dirs.iter().all(|d| d.starts_with(&a)));
}

#[test]
#[ignore = "needs root"]
fn users_only_hear_about_folders_they_can_list() {
    let h = start("users");
    let base = h.base.clone();
    mkdir(&base.join("a"), 0o700, ALICE);
    mkdir(&base.join("b"), 0o700, BOB);
    mkdir(&base.join("b/pub"), 0o755, BOB);
    mkdir(&base.join("shared"), 0o755, 0);
    // Both name the common parent (and even "/") as root.
    let mut sa = subscribe(&h.sock, ALICE, &[&base, Path::new("/")]);
    let mut sb = subscribe(&h.sock, BOB, &[&base]);
    // Carol names Bob's private folder: she cannot list it.
    let mut sc = subscribe(&h.sock, CAROL, &[&base.join("b")]);
    touch(&base.join("a/secret.txt"));
    touch(&base.join("b/secret.txt"));
    touch(&base.join("b/pub/file.txt"));
    touch(&base.join("shared/file.txt"));
    let ga = collect(&mut sa, Duration::from_millis(1500), |_| false);
    let gb = collect(&mut sb, Duration::from_millis(1500), |_| false);
    let gc = collect(&mut sc, Duration::from_millis(500), |_| false);
    let (a, b, shared) = (base.join("a"), base.join("b"), base.join("shared"));
    assert!(
        ga.dirs.contains(&a) && ga.dirs.contains(&shared),
        "{:?}",
        ga.dirs
    );
    assert!(!ga.dirs.iter().any(|d| d.starts_with(&b)), "{:?}", ga.dirs);
    assert!(
        gb.dirs.contains(&b) && gb.dirs.contains(&b.join("pub")),
        "{:?}",
        gb.dirs
    );
    assert!(gb.dirs.contains(&shared));
    assert!(!gb.dirs.iter().any(|d| d.starts_with(&a)), "{:?}", gb.dirs);
    assert!(gc.dirs.is_empty(), "{:?}", gc.dirs);
}

#[test]
#[ignore = "needs root"]
fn refuses_root_and_system_users() {
    let h = start("refuse");
    for uid in [0, 100, 65534] {
        let mut s = connect_as(&h.sock, uid);
        s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        match proto::read_helper_msg(&mut s) {
            Ok(HelperMsg::Error(e)) => assert!(e.contains("regular user"), "{e}"),
            r => panic!("uid {uid}: expected refusal, got {r:?}"),
        }
    }
}

#[test]
#[ignore = "needs root"]
fn drops_misbehaving_clients_and_keeps_serving() {
    let h = start("garbage");
    let a = h.base.join("a");
    mkdir(&a, 0o755, ALICE);
    // Garbage instead of a subscription.
    let mut bad = connect_as(&h.sock, BOB);
    bad.write_all(&[0xff; 64]).unwrap();
    // A second message after the subscription.
    let mut chatty = subscribe(&h.sock, BOB, &[&a]);
    chatty
        .write_all(&proto::HelperMsg::Overflow.encode())
        .unwrap();
    // A good client keeps working.
    let mut good = subscribe(&h.sock, ALICE, &[&a]);
    touch(&a.join("f"));
    let g = collect(&mut good, Duration::from_secs(2), |g| g.dirs.contains(&a));
    assert!(g.dirs.contains(&a));
    for s in [&mut bad, &mut chatty] {
        s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut buf = [0u8; 4096];
        // Closed: EOF (possibly after frames already queued).
        let mut closed = false;
        for _ in 0..100 {
            match std::io::Read::read(s, &mut buf) {
                Ok(0) => {
                    closed = true;
                    break;
                }
                Ok(_) => {}
                Err(e) if e.kind() == ErrorKind::ConnectionReset => {
                    closed = true;
                    break;
                }
                Err(e) => panic!("still open: {e}"),
            }
        }
        assert!(closed);
    }
}

#[test]
#[ignore = "needs root"]
fn privileges_are_dropped_after_setup() {
    let h = start("privs");
    let status = std::fs::read_to_string(format!("/proc/{}/status", h.child.id())).unwrap();
    let field = |n: &str| {
        status
            .lines()
            .find_map(|l| l.strip_prefix(n))
            .unwrap()
            .trim()
            .to_string()
    };
    assert!(
        !field("Uid:").split_whitespace().any(|u| u == "0"),
        "{}",
        field("Uid:")
    );
    assert!(!field("Gid:").split_whitespace().any(|g| g == "0"));
    assert_eq!(field("CapEff:"), "0000000000000004");
    assert_eq!(field("CapPrm:"), "0000000000000004");
    assert_eq!(field("CapBnd:"), "0000000000000004");
    assert_eq!(field("CapAmb:"), "0000000000000000");
    assert_eq!(field("NoNewPrivs:"), "1");
}

#[test]
#[ignore = "needs root"]
fn queue_overflow_tells_clients_to_reconcile() {
    let h = start("overflow");
    let a = h.base.join("a");
    mkdir(&a, 0o755, ALICE);
    let mut s = subscribe(&h.sock, ALICE, &[&a]);
    // Stop the helper so the kernel queue (16384 events) fills up.
    let pid = h.child.id() as i32;
    // SAFETY: signalling our own child process.
    unsafe { libc::kill(pid, libc::SIGSTOP) };
    for i in 0..20_000 {
        touch(&a.join(format!("f{i}")));
    }
    // SAFETY: as above.
    unsafe { libc::kill(pid, libc::SIGCONT) };
    let g = collect(&mut s, Duration::from_secs(10), |g| g.overflows > 0);
    assert!(g.overflows > 0);
}
