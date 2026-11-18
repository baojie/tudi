//! Search timing on a snapshot (P7: keystroke → results < 30 ms p95).
//!
//!     cargo run --release -p tudi-index --example search -- SNAPSHOT [QUERY...]

use std::time::Instant;

use tudi_index::matcher::{Context, SearchData, Weights, search};

fn main() {
    let mut args = std::env::args().skip(1);
    let snap = args.next().expect("SNAPSHOT");
    let queries: Vec<String> = args.collect();
    let rss = || {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("VmRSS:"))
                    .and_then(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok())
            })
            .map_or(0.0, |kb| kb / 1024.0)
    };
    let ix = tudi_index::snapshot::load(std::path::Path::new(&snap)).unwrap();
    eprintln!("rss after load: {:.0} MB", rss());
    let t = Instant::now();
    let sd = SearchData::build(&ix);
    eprintln!("search data: {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
    let t = Instant::now();
    sd.warm(&ix); // the daemon does this in the background after loading
    eprintln!("typo vocabulary: {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
    eprintln!("rss after search data + vocabulary: {:.0} MB", rss());
    tudi_index::release_free_memory();
    eprintln!("rss after trim: {:.0} MB", rss());
    let default = [
        "r",
        "re",
        "rep",
        "repo",
        "report",
        "report 2024",
        "acme",
        "hthd",
        "合同",
        "invocie",
        "main rs",
        "zzzz",
    ];
    let qs: Vec<String> = if queries.is_empty() {
        default.iter().map(|s| s.to_string()).collect()
    } else {
        queries
    };
    let mut times = Vec::new();
    for q in &qs {
        for _ in 0..5 {
            let t = Instant::now();
            let hits = search(&ix, &sd, q, 50, &Weights::default(), &Context::default());
            let ms = t.elapsed().as_secs_f64() * 1e3;
            times.push(ms);
            if times.len() % 5 == 0 {
                let first = hits
                    .first()
                    .map(|h| ix.path(h.node).display().to_string())
                    .unwrap_or_default();
                eprintln!("{q:>12}: {ms:6.1} ms, {} hits, top: {first}", hits.len());
            }
        }
    }
    tudi_index::release_free_memory();
    eprintln!("rss after queries + trim: {:.0} MB", rss());
    times.sort_by(f64::total_cmp);
    eprintln!(
        "p50 {:.1} ms, p95 {:.1} ms, max {:.1} ms",
        times[times.len() / 2],
        times[times.len() * 95 / 100],
        times[times.len() - 1]
    );
}
