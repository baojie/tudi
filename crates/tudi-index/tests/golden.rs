//! Search-quality golden set (PLAN §9.3, M6 acceptance): ≥ 90 % of cases
//! have the expected item in the top 3, 100 % in the top 10. Prints a
//! report (`cargo test -p tudi-index --test golden -- --nocapture`).

// The printed report is the point of this test (PLAN §9.3).
#![allow(clippy::print_stdout)]

use std::path::Path;

use tudi_index::matcher::{Context, SearchData, Weights, search};
use tudi_index::{Entry, Index, Kind};

/// A plausible home folder: projects, documents, downloads, Chinese names,
/// many look-alikes to rank against.
const TREE: &[&str] = &[
    "projects/acme/README.md",
    "projects/acme/src/main.rs",
    "projects/acme/src/lib.rs",
    "projects/acme/docs/design.md",
    "projects/acme/reports/2024季报.pdf",
    "projects/acme/reports/2025季报.pdf",
    "projects/acme/reports/annual-report-2024.pdf",
    "projects/acme/contracts/合同核对.xlsx",
    "projects/acme/contracts/合同-终版.docx",
    "projects/tudi/docs/PLAN.md",
    "projects/tudi/crates/tudi-core/src/tree.rs",
    "projects/tudi/crates/tudi-gui/src/main.rs",
    "projects/tudi/Cargo.toml",
    "projects/website/index.html",
    "projects/website/assets/logo.svg",
    "projects/website/package.json",
    "Documents/finance/invoice-2025-03.pdf",
    "Documents/finance/invoice-2024-11.pdf",
    "Documents/finance/budget-2025.xlsx",
    "Documents/finance/tax-return-2024.pdf",
    "Documents/finance/发票/2025年3月发票.pdf",
    "Documents/personal/passport-scan.jpg",
    "Documents/personal/resume-2025.docx",
    "Documents/personal/简历2025.docx",
    "Documents/会议纪要/2025-03-12 周会.md",
    "Documents/会议纪要/2025-04-02 季度会.md",
    "Documents/meeting-notes/standup-2025-03-12.md",
    "Documents/年报/2024年度报告.pdf",
    "Documents/年报/2023年度报告.pdf",
    "Downloads/ubuntu-26.04-desktop-amd64.iso",
    "Downloads/report.pdf",
    "Downloads/report (1).pdf",
    "Downloads/Screenshot from 2025-03-01.png",
    "Downloads/kitty-0.40.tar.xz",
    "Downloads/合同扫描件.pdf",
    "Pictures/2024/vacation/beach.jpg",
    "Pictures/2024/vacation/sunset.jpg",
    "Pictures/2025/family/birthday.jpg",
    "Pictures/wallpapers/mountains.png",
    "Music/jazz/kind-of-blue.flac",
    "Videos/talks/rustconf-2024.mp4",
    "bin/backup.sh",
    "bin/sync-photos.py",
    ".config/kitty/kitty.conf",
    ".config/zed/settings.json",
    ".bashrc",
    "notes/todo.md",
    "notes/ideas.md",
    "notes/读书笔记/三体.md",
    "notes/读书笔记/人类简史.md",
    "archive/old-projects/acme-2019/report.doc",
    "archive/old-projects/legacy/main.c",
    "work/clients/globex/proposal-v3.pptx",
    "work/clients/globex/proposal-v2.pptx",
    "work/clients/initech/contract-signed.pdf",
    "work/presentations/quarterly-review-q1.pptx",
];

struct Case {
    query: &'static str,
    expect: &'static str,
    /// Must be in the top 3 (otherwise top 10 is enough).
    top3: bool,
    note: &'static str,
}

const fn c(query: &'static str, expect: &'static str, top3: bool, note: &'static str) -> Case {
    Case {
        query,
        expect,
        top3,
        note,
    }
}

const CASES: &[Case] = &[
    // English names.
    c(
        "plan",
        "projects/tudi/docs/PLAN.md",
        true,
        "case-insensitive",
    ),
    c("budget", "Documents/finance/budget-2025.xlsx", true, "word"),
    c(
        "passport",
        "Documents/personal/passport-scan.jpg",
        true,
        "word",
    ),
    c(
        "resume",
        "Documents/personal/resume-2025.docx",
        true,
        "word",
    ),
    c(
        "kitty conf",
        ".config/kitty/kitty.conf",
        true,
        "below a hidden folder, two tokens",
    ),
    c("sunset", "Pictures/2024/vacation/sunset.jpg", true, "word"),
    c("rustconf", "Videos/talks/rustconf-2024.mp4", true, "word"),
    c(
        "ubuntu iso",
        "Downloads/ubuntu-26.04-desktop-amd64.iso",
        true,
        "name + extension",
    ),
    c("backup", "bin/backup.sh", true, "script"),
    c(
        "sync photos",
        "bin/sync-photos.py",
        true,
        "two words of one name",
    ),
    c("logo", "projects/website/assets/logo.svg", true, "word"),
    c(
        "tree rs",
        "projects/tudi/crates/tudi-core/src/tree.rs",
        true,
        "name + extension",
    ),
    c(
        "cargo toml",
        "projects/tudi/Cargo.toml",
        true,
        "name + extension",
    ),
    c("todo", "notes/todo.md", true, "word"),
    c(
        "standup",
        "Documents/meeting-notes/standup-2025-03-12.md",
        true,
        "word",
    ),
    c(
        "tax return",
        "Documents/finance/tax-return-2024.pdf",
        true,
        "two words",
    ),
    c(
        "kind of blue",
        "Music/jazz/kind-of-blue.flac",
        true,
        "three words",
    ),
    // Path fragments and order independence.
    c(
        "acme main",
        "projects/acme/src/main.rs",
        true,
        "folder + name",
    ),
    c("main acme", "projects/acme/src/main.rs", true, "order-free"),
    c(
        "tudi main",
        "projects/tudi/crates/tudi-gui/src/main.rs",
        true,
        "folder + name",
    ),
    c(
        "globex v3",
        "work/clients/globex/proposal-v3.pptx",
        true,
        "folder + name fragment",
    ),
    c(
        "initech contract",
        "work/clients/initech/contract-signed.pdf",
        true,
        "folder + name",
    ),
    c(
        "vacation beach",
        "Pictures/2024/vacation/beach.jpg",
        true,
        "folder + name",
    ),
    c(
        "acme design",
        "projects/acme/docs/design.md",
        true,
        "folder + name",
    ),
    c(
        "legacy main",
        "archive/old-projects/legacy/main.c",
        true,
        "folder + name",
    ),
    c(
        "invoice 2025",
        "Documents/finance/invoice-2025-03.pdf",
        true,
        "name + number",
    ),
    c(
        "annual report",
        "projects/acme/reports/annual-report-2024.pdf",
        true,
        "two words",
    ),
    c(
        "quarterly review",
        "work/presentations/quarterly-review-q1.pptx",
        true,
        "two words",
    ),
    // Chinese names.
    c(
        "季报",
        "projects/acme/reports/2025季报.pdf",
        false,
        "CJK word",
    ),
    c(
        "acme 季报",
        "projects/acme/reports/2024季报.pdf",
        false,
        "folder + CJK",
    ),
    c(
        "合同核对",
        "projects/acme/contracts/合同核对.xlsx",
        true,
        "CJK name",
    ),
    c("简历", "Documents/personal/简历2025.docx", true, "CJK name"),
    c("三体", "notes/读书笔记/三体.md", true, "CJK name"),
    c(
        "周会",
        "Documents/会议纪要/2025-03-12 周会.md",
        true,
        "CJK name",
    ),
    c(
        "发票 2025",
        "Documents/finance/发票/2025年3月发票.pdf",
        true,
        "CJK + number",
    ),
    c(
        "年度报告 2024",
        "Documents/年报/2024年度报告.pdf",
        true,
        "CJK + number",
    ),
    // Pinyin.
    c(
        "hthd",
        "projects/acme/contracts/合同核对.xlsx",
        true,
        "initials",
    ),
    c(
        "hetong",
        "projects/acme/contracts/合同-终版.docx",
        false,
        "full pinyin",
    ),
    c(
        "jianli",
        "Documents/personal/简历2025.docx",
        true,
        "full pinyin",
    ),
    c("santi", "notes/读书笔记/三体.md", true, "full pinyin"),
    c("dsbj", "notes/读书笔记", true, "initials of a folder"),
    c("hyjy", "Documents/会议纪要", true, "initials of a folder"),
    c("rlja", "notes/读书笔记/人类简史.md", true, "initials"),
    // Misspellings (typo fallback).
    c(
        "invocie",
        "Documents/finance/invoice-2025-03.pdf",
        false,
        "transposition",
    ),
    c(
        "pasport",
        "Documents/personal/passport-scan.jpg",
        true,
        "missing letter",
    ),
    c(
        "propsal",
        "work/clients/globex/proposal-v3.pptx",
        false,
        "missing letter",
    ),
    c(
        "screnshot",
        "Downloads/Screenshot from 2025-03-01.png",
        true,
        "missing letter",
    ),
];

fn build() -> Index {
    let mut ix = Index::new();
    let root = ix.add_root(Path::new("/home/u"), 0);
    for p in TREE {
        let comps: Vec<&str> = p.split('/').collect();
        let mut cur = root;
        for (k, c) in comps.iter().enumerate() {
            let last = k + 1 == comps.len();
            let kind = if last { Kind::File } else { Kind::Dir };
            let found = ix.children(cur).find(|&x| ix.name(x) == c.as_bytes());
            cur = match found {
                Some(x) => x,
                None => ix.push(
                    cur,
                    &Entry {
                        name: c.as_bytes().to_vec(),
                        kind,
                        size: 1000,
                        mtime: 1_750_000_000,
                    },
                    0,
                ),
            };
        }
    }
    ix
}

#[test]
fn golden_set() {
    assert!(CASES.len() >= 40, "the golden set needs ≥ 40 cases");
    let ix = build();
    let sd = SearchData::build(&ix);
    let ctx = Context::default();
    let mut top3 = 0;
    let mut top10 = 0;
    let mut failures = Vec::new();
    for case in CASES {
        let hits = search(&ix, &sd, case.query, 10, &Weights::default(), &ctx);
        let want = Path::new("/home/u").join(case.expect);
        let rank = hits.iter().position(|h| ix.path(h.node) == want);
        match rank {
            Some(r) if r < 3 => {
                top3 += 1;
                top10 += 1;
            }
            Some(_) => top10 += 1,
            None => {}
        }
        let ok = match rank {
            Some(r) => !case.top3 || r < 3,
            None => false,
        };
        let shown: Vec<String> = hits
            .iter()
            .take(3)
            .map(|h| {
                ix.path(h.node)
                    .strip_prefix("/home/u")
                    .unwrap()
                    .display()
                    .to_string()
            })
            .collect();
        println!(
            "{} {:<22} rank {:<4} ({}) top: {}",
            if ok { "ok  " } else { "FAIL" },
            case.query,
            rank.map_or("-".into(), |r| (r + 1).to_string()),
            case.note,
            shown.join(" | ")
        );
        if !ok {
            failures.push(case.query);
        }
    }
    let n = CASES.len();
    println!(
        "golden set: {n} cases, top 3: {top3} ({:.0} %), top 10: {top10} ({:.0} %)",
        top3 as f64 * 100.0 / n as f64,
        top10 as f64 * 100.0 / n as f64
    );
    assert!(top3 * 10 >= n * 9, "top 3 below 90 %: {failures:?}");
    assert_eq!(top10, n, "not all in the top 10: {failures:?}");
    assert!(
        failures.is_empty(),
        "cases below their target: {failures:?}"
    );
}
