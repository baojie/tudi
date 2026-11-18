//! M2 benches: list + sort (no stat) for 10k / 100k entries, metadata for
//! 10k. Uses the fixtures from `benches/fixtures/make_tree.py`, found in
//! `$TUDI_FIXTURES` or `~/.cache/tudi-fixtures`; skipped if missing.
//!
//! Warm cache: the first iterations warm the dentry/inode caches.

use std::path::PathBuf;
use std::sync::{Arc, mpsc};

use criterion::{Criterion, criterion_group, criterion_main};
use tudi_core::listing::meta::{MetaFetcher, MetaTable};
use tudi_core::listing::sort::{NameRank, SortSpec, order};
use tudi_core::listing::{Lister, Listing};

fn fixture(name: &str) -> Option<PathBuf> {
    let base = std::env::var_os("TUDI_FIXTURES")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache/tudi-fixtures"))
        })?;
    let p = base.join(name);
    if p.is_dir() {
        Some(p)
    } else {
        eprintln!("skipping {name}: fixture not found at {}", p.display());
        None
    }
}

fn list_and_sort(lister: &mut Lister, path: &std::path::Path) -> (Listing, Vec<u32>) {
    let l = lister.list(path, 0).unwrap();
    let rank = NameRank::new(&l);
    let o = order(&l, &rank, &MetaTable::new(l.len()), SortSpec::default());
    (l, o)
}

fn bench_list_sort(c: &mut Criterion) {
    for name in ["flat-10k", "flat-100k"] {
        let Some(path) = fixture(name) else { continue };
        let mut lister = Lister::new();
        c.bench_function(&format!("list+sort/{name}"), |b| {
            b.iter(|| list_and_sort(&mut lister, &path))
        });
        c.bench_function(&format!("list/{name}"), |b| {
            b.iter(|| lister.list(&path, 0).unwrap())
        });
        let l = lister.list(&path, 0).unwrap();
        c.bench_function(&format!("name-rank/{name}"), |b| {
            b.iter(|| NameRank::new(&l))
        });
        c.bench_function(&format!("name-rank-pinyin/{name}"), |b| {
            b.iter(|| NameRank::with_pinyin(&l, true))
        });
        let rank = NameRank::new(&l);
        let metas = MetaTable::new(l.len());
        c.bench_function(&format!("order/{name}"), |b| {
            b.iter(|| order(&l, &rank, &metas, SortSpec::default()))
        });
    }
}

fn bench_metadata(c: &mut Criterion) {
    let Some(path) = fixture("flat-10k") else {
        return;
    };
    let l = Arc::new(Lister::new().list(&path, 0).unwrap());
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
    c.bench_function("metadata/flat-10k", |b| {
        b.iter(|| {
            let (tx, rx) = mpsc::channel();
            let tx = std::sync::Mutex::new(tx);
            let _f = MetaFetcher::start(
                l.clone(),
                &[],
                workers,
                Arc::new(move |batch| tx.lock().unwrap().send(batch.complete).unwrap()),
            );
            while !rx.recv().unwrap() {}
        })
    });
}

criterion_group!(benches, bench_list_sort, bench_metadata);
criterion_main!(benches);
