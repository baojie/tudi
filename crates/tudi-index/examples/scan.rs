//! Scan a folder into an index and report time, size and RSS (P8, P9).
//!
//!     cargo run --release -p tudi-index --example scan -- PATH [SNAPSHOT]

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

fn rss_mb() -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok())
        })
        .map_or(0.0, |kb| kb / 1024.0)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().expect("PATH"));
    let snap = args.next().map(PathBuf::from);
    let t = Instant::now();
    let none = AtomicBool::new(false);
    let ix = tudi_index::scan::build(
        &[root],
        &tudi_index::scan::Excludes::defaults(),
        &none,
        |_| {},
    )
    .expect("scan");
    let scan = t.elapsed();
    eprintln!(
        "{{\"entries\":{},\"scan_s\":{:.2},\"rss_mb\":{:.1},\"names_mb\":{:.1}}}",
        ix.len(),
        scan.as_secs_f64(),
        rss_mb(),
        ix.names.len() as f64 / 1e6
    );
    if let Some(p) = snap {
        let t = Instant::now();
        tudi_index::snapshot::save(&ix, &p).unwrap();
        let save = t.elapsed();
        drop(ix);
        let t = Instant::now();
        let ix = tudi_index::snapshot::load(&p).unwrap();
        eprintln!(
            "{{\"save_ms\":{:.0},\"load_ms\":{:.0},\"entries\":{}}}",
            save.as_secs_f64() * 1e3,
            t.elapsed().as_secs_f64() * 1e3,
            ix.len()
        );
    }
}
