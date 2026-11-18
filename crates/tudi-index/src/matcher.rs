//! Memory search over the index (PLAN §5.5, M6).
//!
//! A query is whitespace-separated tokens, order-free. Each token is scored
//! against every node's *own name* (and pinyin alternates of CJK names)
//! with `nucleo-matcher`'s fuzzy (subsequence) scoring. Tokens may match
//! different levels of a path: a forward pass in parent-before-child order
//! propagates `cum_mask(n) = cum_mask(parent) | own_mask(n)`. A node is a
//! hit when every token matched it or an ancestor, and at least one token
//! matched its own name (otherwise every file below a matching folder would
//! be a hit).
//!
//! Speed: a 64-bit character bag per name (and its alternates) rejects most
//! nodes before any fuzzy scoring; scoring runs in parallel (rayon); top-K
//! per chunk, then merged.
//!
//! Typo fallback (ADR-013): when a token of ≥ 4 characters matches fewer
//! than `TYPO_MIN_HITS` names, names containing a word within
//! Damerau-Levenshtein distance 1 (2 for ≥ 8 characters) also count, scored
//! below exact matches.

use std::collections::HashMap;

use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use pinyin::ToPinyin;
use rayon::prelude::*;

use crate::{Index, NONE, flags};

/// Below this many own-name matches a token also gets typo matches.
pub const TYPO_MIN_HITS: usize = 5;

/// Ranking weights (tuned against the golden set).
#[derive(Clone, Copy, Debug)]
pub struct Weights {
    /// Per-token score when it matched an ancestor, relative to own name.
    pub ancestor: f32,
    /// Score of a typo match relative to an exact one.
    pub typo: f32,
    /// Bonus when the whole name starts with the token.
    pub prefix: f32,
    /// Penalty per path level below the root.
    pub depth: f32,
    /// Multiplier range from frecency of the folder (or nearest ancestor).
    pub frecency: f32,
    /// Bonus for recently modified items (decays over ~a month).
    pub recency: f32,
    /// Folders rank slightly above files on equal scores.
    pub dir: f32,
}

impl Default for Weights {
    fn default() -> Self {
        Self {
            ancestor: 0.35,
            typo: 0.45,
            prefix: 12.0,
            depth: 0.8,
            frecency: 1.0,
            recency: 6.0,
            dir: 2.0,
        }
    }
}

/// One search result.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub node: u32,
    pub score: f32,
    /// Matched character positions in the node's own name (chars), for
    /// highlighting; empty for pinyin / typo matches.
    pub indices: Vec<u32>,
}

/// Per-index search data built once (and after updates): character bags
/// and pinyin alternates.
#[derive(Default)]
pub struct SearchData {
    bags: Vec<u64>,
    /// Pinyin alternates ("hetonghedui", "hthd") of CJK names.
    alt: HashMap<u32, Vec<String>>,
    /// Lowercased words of names with the nodes containing them, for the
    /// typo fallback (built lazily, or by [`SearchData::warm`]).
    vocab: std::sync::OnceLock<Vec<VocabWord>>,
}

fn bag_byte(b: u8) -> u64 {
    let b = b.to_ascii_lowercase();
    let bit = match b {
        b'a'..=b'z' => b - b'a',
        b'0'..=b'9' => 26 + b - b'0',
        0..=0x7f => 36 + b % 16,
        _ => 52 + b % 12,
    };
    1u64 << bit
}

fn bag(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0, |m, &b| m | bag_byte(b))
}

/// Full pinyin and initials of a name with CJK characters; non-CJK
/// characters are kept as they are (lowercased).
pub fn pinyin_alternates(name: &str) -> Vec<String> {
    let mut full = String::new();
    let mut initials = String::new();
    for (c, p) in name.chars().zip(name.to_pinyin()) {
        match p {
            Some(p) => {
                full.push_str(p.plain());
                initials.push_str(p.first_letter());
            }
            None => {
                for l in c.to_lowercase() {
                    full.push(l);
                    initials.push(l);
                }
            }
        }
    }
    vec![full, initials]
}

/// Split a name into lowercase words (separators, case and digit changes).
pub fn words(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev: Option<char> = None;
    for c in name.chars() {
        let boundary = !c.is_alphanumeric()
            || prev.is_some_and(|p| {
                (p.is_lowercase() && c.is_uppercase()) || (p.is_ascii_digit() != c.is_ascii_digit())
            });
        if boundary && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        if c.is_alphanumeric() {
            cur.extend(c.to_lowercase());
        }
        prev = Some(c);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Damerau-Levenshtein (optimal string alignment) distance, capped: returns
/// `max + 1` as soon as it exceeds `max`.
pub fn osa_distance(a: &str, b: &str, max: usize) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len().abs_diff(b.len()) > max {
        return max + 1;
    }
    let (n, m) = (a.len(), b.len());
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=n {
        let mut row_min = usize::MAX;
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut v = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                v = v.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = v;
            row_min = row_min.min(v);
        }
        if row_min > max {
            return max + 1;
        }
    }
    d[n][m]
}

impl SearchData {
    pub fn build(ix: &Index) -> Self {
        let bags: Vec<u64> = (0..ix.nodes.len() as u32)
            .into_par_iter()
            .map(|i| bag(ix.name(i)))
            .collect();
        let alt: HashMap<u32, Vec<String>> = ix
            .nodes
            .par_iter()
            .enumerate()
            .filter(|(_, n)| n.flags & flags::CJK != 0)
            .map(|(i, _)| {
                let name = String::from_utf8_lossy(ix.name(i as u32));
                (i as u32, pinyin_alternates(&name))
            })
            .collect();
        let mut bags = bags;
        for (&i, alts) in &alt {
            for a in alts {
                bags[i as usize] |= bag(a.as_bytes());
            }
        }
        Self {
            bags,
            alt,
            vocab: std::sync::OnceLock::new(),
        }
    }

    /// Build the typo vocabulary now (e.g. on a background thread after
    /// loading), so the first typo query does not pay for it.
    pub fn warm(&self, ix: &Index) {
        self.vocab(ix);
    }

    fn vocab(&self, ix: &Index) -> &[VocabWord] {
        self.vocab.get_or_init(|| {
            let mut v: HashMap<String, Vec<u32>> = HashMap::new();
            for i in 0..ix.nodes.len() as u32 {
                if ix.nodes[i as usize].is_dead() || ix.nodes[i as usize].parent == NONE {
                    continue;
                }
                for w in words(&String::from_utf8_lossy(ix.name(i))) {
                    if w.len() >= 3 {
                        v.entry(w).or_default().push(i);
                    }
                }
            }
            v.into_iter()
                .map(|(word, nodes)| VocabWord {
                    bag: bag(word.as_bytes()),
                    chars: word.chars().count().min(255) as u8,
                    word,
                    nodes,
                })
                .collect()
        })
    }
}

/// A name word for the typo fallback, with cheap prefilters.
pub struct VocabWord {
    word: String,
    bag: u64,
    chars: u8,
    nodes: Vec<u32>,
}

/// Extra context for ranking.
#[derive(Default)]
pub struct Context<'a> {
    /// Frecency weight per folder node (0..=1, normalized).
    pub frecency: Option<&'a HashMap<u32, f32>>,
    /// Now, Unix seconds (for the recency bonus).
    pub now: i64,
    /// Include hidden entries even if no token starts with a dot.
    pub show_hidden: bool,
}

/// Case-insensitive "starts with" without allocating.
fn starts_with_ci(name: &str, lower_prefix: &str) -> bool {
    let mut it = name.chars().flat_map(char::to_lowercase);
    lower_prefix.chars().all(|c| it.next() == Some(c))
}

/// Search `ix`. Returns at most `limit` hits, best first.
pub fn search(
    ix: &Index,
    sd: &SearchData,
    query: &str,
    limit: usize,
    w: &Weights,
    ctx: &Context,
) -> Vec<Hit> {
    let tokens: Vec<String> = query
        .split_whitespace()
        .take(15)
        .map(|t| t.to_lowercase())
        .collect();
    if tokens.is_empty() || ix.nodes.is_empty() {
        return Vec::new();
    }
    let all: u64 = (1u64 << tokens.len()) - 1;
    let token_lens: Vec<f32> = tokens.iter().map(|t| t.chars().count() as f32).collect();
    let atoms: Vec<Atom> = tokens
        .iter()
        .map(|t| {
            Atom::new(
                t,
                CaseMatching::Ignore,
                Normalization::Smart,
                AtomKind::Fuzzy,
                false,
            )
        })
        .collect();
    let token_bags: Vec<u64> = tokens.iter().map(|t| bag(t.as_bytes())).collect();
    let want_hidden = ctx.show_hidden || tokens.iter().any(|t| t.starts_with('.'));

    // Phase 1 (parallel): own-name matches into dense arrays (zeroed
    // allocations are lazily mapped, so untouched pages cost nothing).
    let n = ix.nodes.len();
    let mut masks = vec![0u16; n];
    let mut scores = vec![0f32; n];
    masks
        .par_chunks_mut(8192)
        .zip(scores.par_chunks_mut(8192))
        .enumerate()
        .for_each_init(
            || (Matcher::new(Config::DEFAULT), Vec::<char>::new()),
            |(m, buf), (chunk, (mk, sc))| {
                let base = chunk * 8192;
                for k in 0..mk.len() {
                    let i = (base + k) as u32;
                    let node = &ix.nodes[i as usize];
                    if node.is_dead() || node.parent == NONE {
                        continue;
                    }
                    let nb = sd.bags[i as usize];
                    if token_bags.iter().all(|&tb| tb & !nb != 0) {
                        continue;
                    }
                    let name = String::from_utf8_lossy(ix.name(i));
                    for (t, atom) in atoms.iter().enumerate() {
                        if token_bags[t] & !nb != 0 {
                            continue;
                        }
                        let tlen = token_lens[t];
                        let mut best = atom
                            .score(Utf32Str::new(&name, buf), m)
                            .map(|s| f32::from(s) / tlen);
                        if let Some(alts) = sd.alt.get(&i) {
                            for a in alts {
                                if let Some(s) = atom.score(Utf32Str::new(a, buf), m) {
                                    let s = f32::from(s) / tlen * 0.9;
                                    best = Some(best.map_or(s, |b: f32| b.max(s)));
                                }
                            }
                        }
                        if let Some(mut s) = best {
                            if starts_with_ci(&name, &tokens[t]) {
                                s += w.prefix;
                            }
                            mk[k] |= 1 << t;
                            sc[k] += s;
                        }
                    }
                }
            },
        );

    // Typo fallback for tokens with too few own-name matches.
    for (t, tok) in tokens.iter().enumerate() {
        let len = tok.chars().count();
        if len < 4 || !tok.chars().all(char::is_alphanumeric) {
            continue;
        }
        let bit = 1u16 << t;
        let hits = masks.par_iter().filter(|&&m| m & bit != 0).count();
        if hits >= TYPO_MIN_HITS {
            continue;
        }
        let max = if len >= 8 { 2 } else { 1 };
        let tbag = token_bags[t];
        // Each edit changes at most two bits of the character bag.
        let close: Vec<&VocabWord> = sd
            .vocab(ix)
            .par_iter()
            .filter(|v| {
                usize::from(v.chars).abs_diff(len) <= max
                    && (v.bag ^ tbag).count_ones() as usize <= 2 * max
                    && v.word != *tok
                    && osa_distance(&v.word, tok, max) <= max
            })
            .collect();
        for v in close {
            {
                let nodes = &v.nodes;
                for &i in nodes {
                    if masks[i as usize] & bit == 0 {
                        masks[i as usize] |= bit;
                        scores[i as usize] += 16.0 * w.typo;
                    }
                }
            }
        }
    }

    // Phase 2 (parallel): every node with an own match walks up its
    // ancestors to complete the token mask, sum ancestor scores and find
    // the nearest frecent folder.
    let all = all as u16;
    let mut hits: Vec<Hit> = (0..n)
        .into_par_iter()
        .filter_map(|i| {
            let own = masks[i];
            if own == 0 {
                return None;
            }
            let node = &ix.nodes[i];
            if !want_hidden && node.flags & flags::HIDDEN != 0 {
                return None;
            }
            let mut cum = own;
            let mut anc = 0f32;
            let mut depth = 0u32;
            let mut frec = ctx
                .frecency
                .and_then(|f| f.get(&(i as u32)).copied())
                .unwrap_or(0.0);
            let mut decay = 1.0f32;
            let mut p = node.parent;
            while p != NONE {
                let pm = masks[p as usize];
                if pm != 0 {
                    cum |= pm;
                    anc += scores[p as usize];
                }
                if let Some(f) = ctx.frecency.and_then(|f| f.get(&p)) {
                    decay *= 0.7;
                    frec = frec.max(f * decay);
                }
                depth += 1;
                p = ix.nodes[p as usize].parent;
            }
            if cum != all {
                return None;
            }
            let mut score = scores[i] + anc * w.ancestor - depth as f32 * w.depth;
            if node.is_dir() {
                score += w.dir;
            }
            score *= 1.0 + frec * w.frecency;
            if ctx.now > 0 {
                let age_days = (ctx.now - crate::unpack_mtime(node.mtime)).max(0) as f32 / 86_400.0;
                score += w.recency / (1.0 + age_days / 30.0);
            }
            Some(Hit {
                node: i as u32,
                score,
                indices: Vec::new(),
            })
        })
        .collect();
    let limit = limit.max(1);
    if hits.len() > limit {
        hits.select_nth_unstable_by(limit - 1, |a, b| b.score.total_cmp(&a.score));
        hits.truncate(limit);
    }
    hits.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.node.cmp(&b.node)));
    // Highlight positions for the shown hits only.
    let mut m = Matcher::new(Config::DEFAULT);
    let mut buf = Vec::new();
    for h in &mut hits {
        let name = String::from_utf8_lossy(ix.name(h.node));
        for atom in &atoms {
            let mut idx = Vec::new();
            if atom
                .indices(Utf32Str::new(&name, &mut buf), &mut m, &mut idx)
                .is_some()
            {
                h.indices.extend(idx);
            }
        }
        h.indices.sort_unstable();
        h.indices.dedup();
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Entry, Kind};
    use std::path::Path;

    fn build(paths: &[&str]) -> Index {
        let mut ix = Index::new();
        let root = ix.add_root(Path::new("/r"), 0);
        for p in paths {
            let mut cur = root;
            let comps: Vec<&str> = p.split('/').collect();
            for (k, c) in comps.iter().enumerate() {
                let kind = if k + 1 == comps.len() && c.contains('.') {
                    Kind::File
                } else {
                    Kind::Dir
                };
                let found = ix.children(cur).find(|&x| ix.name(x) == c.as_bytes());
                cur = match found {
                    Some(x) => x,
                    None => ix.push(
                        cur,
                        &Entry {
                            name: c.as_bytes().to_vec(),
                            kind,
                            size: 1,
                            mtime: 1_700_000_000,
                        },
                        0,
                    ),
                };
            }
        }
        ix
    }

    fn top(ix: &Index, q: &str) -> Vec<String> {
        let sd = SearchData::build(ix);
        search(ix, &sd, q, 10, &Weights::default(), &Context::default())
            .iter()
            .map(|h| ix.path(h.node).to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn helpers() {
        assert_eq!(
            pinyin_alternates("合同核对.xlsx"),
            ["hetonghedui.xlsx", "hthd.xlsx"]
        );
        assert_eq!(
            words("Invoice-2025_finalDraft.pdf"),
            ["invoice", "2025", "final", "draft", "pdf"]
        );
        assert_eq!(osa_distance("invocie", "invoice", 1), 1);
        assert_eq!(osa_distance("reprot", "report", 1), 1);
        assert_eq!(osa_distance("abc", "xyz", 1), 2);
    }

    #[test]
    fn plan_queries() {
        let ix = build(&[
            "work/合同核对.xlsx",
            "acme/reports/2024季报.pdf",
            "other/2024季报.pdf",
            "finance/invoice-2025.pdf",
            "finance/invoice-2024.pdf",
            "notes/plan.md",
            "tudi/plan/design.md",
        ]);
        assert_eq!(top(&ix, "hthd")[0], "/r/work/合同核对.xlsx");
        assert_eq!(top(&ix, "hetong")[0], "/r/work/合同核对.xlsx");
        // A folder token plus a name token, in any order.
        assert_eq!(top(&ix, "acme 季报"), ["/r/acme/reports/2024季报.pdf"]);
        assert_eq!(top(&ix, "季报 acme"), ["/r/acme/reports/2024季报.pdf"]);
        // Typo fallback.
        let r = top(&ix, "invocie");
        assert!(r.iter().any(|p| p.ends_with("invoice-2025.pdf")), "{r:?}");
        // Exact hits come before typo hits.
        let ix2 = build(&["a/invoice.pdf", "b/invocie-old.pdf"]);
        let r = top(&ix2, "invocie");
        assert_eq!(r[0], "/r/b/invocie-old.pdf");
        // Path fragments.
        assert_eq!(top(&ix, "tudi plan")[0], "/r/tudi/plan");
    }

    #[test]
    fn hidden_and_empty() {
        let ix = build(&["a/.secret.txt", "a/visible.txt"]);
        assert!(top(&ix, "secret").is_empty());
        assert_eq!(top(&ix, ".secret"), ["/r/a/.secret.txt"]);
        assert!(top(&ix, "   ").is_empty());
        assert!(top(&ix, "zzzzqqq").is_empty());
    }
}
