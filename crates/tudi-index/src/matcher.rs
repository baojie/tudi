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

use crate::query::{KindFilter, Query, extension, parse};
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
    alt: Alternates,
    /// Lowercased words of names with the nodes containing them, for the
    /// typo fallback (built lazily, or by [`SearchData::warm`]).
    vocab: std::sync::OnceLock<Vocab>,
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

/// Pinyin alternates of CJK names, compactly: sorted node ids with
/// offsets into one text arena (full pinyin, then initials).
#[derive(Default)]
struct Alternates {
    nodes: Vec<u32>,
    /// Per node: start of the full form; the initials follow at `mid`.
    spans: Vec<(u32, u32, u32)>,
    text: String,
}

impl Alternates {
    fn get(&self, node: u32) -> Option<[&str; 2]> {
        let k = self.nodes.binary_search(&node).ok()?;
        let (a, mid, b) = self.spans[k];
        Some([
            &self.text[a as usize..mid as usize],
            &self.text[mid as usize..b as usize],
        ])
    }
}

/// Name words for the typo fallback: one text arena, cheap prefilters per
/// word, and the nodes containing each word in one postings array.
#[derive(Default)]
struct Vocab {
    text: String,
    words: Vec<VocabWord>,
    postings: Vec<u32>,
}

struct VocabWord {
    off: u32,
    len: u16,
    chars: u8,
    bag: u64,
    post_off: u32,
    post_len: u32,
}

impl Vocab {
    fn word(&self, w: &VocabWord) -> &str {
        &self.text[w.off as usize..w.off as usize + usize::from(w.len)]
    }
}

impl SearchData {
    pub fn build(ix: &Index) -> Self {
        let mut bags: Vec<u64> = (0..ix.nodes.len() as u32)
            .into_par_iter()
            .map(|i| bag(ix.name(i)))
            .collect();
        let mut pairs: Vec<(u32, [String; 2])> = ix
            .nodes
            .par_iter()
            .enumerate()
            .filter(|(_, n)| n.flags & flags::CJK != 0 && !n.is_dead())
            .map(|(i, _)| {
                let name = String::from_utf8_lossy(ix.name(i as u32));
                let mut v = pinyin_alternates(&name);
                let init = v.pop().unwrap_or_default();
                let full = v.pop().unwrap_or_default();
                (i as u32, [full, init])
            })
            .collect();
        pairs.sort_unstable_by_key(|p| p.0);
        let mut alt = Alternates::default();
        alt.nodes.reserve_exact(pairs.len());
        alt.spans.reserve_exact(pairs.len());
        for (i, [full, init]) in pairs {
            bags[i as usize] |= bag(full.as_bytes()) | bag(init.as_bytes());
            let a = alt.text.len() as u32;
            alt.text.push_str(&full);
            let mid = alt.text.len() as u32;
            alt.text.push_str(&init);
            alt.nodes.push(i);
            alt.spans.push((a, mid, alt.text.len() as u32));
        }
        alt.text.shrink_to_fit();
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

    fn vocab(&self, ix: &Index) -> &Vocab {
        self.vocab.get_or_init(|| {
            // (word, node) pairs, sorted, then grouped.
            let mut pairs: Vec<(String, u32)> = (0..ix.nodes.len() as u32)
                .into_par_iter()
                .filter(|&i| {
                    let n = &ix.nodes[i as usize];
                    !n.is_dead() && n.parent != NONE
                })
                .flat_map_iter(|i| {
                    words(&String::from_utf8_lossy(ix.name(i)))
                        .into_iter()
                        .filter(|w| w.len() >= 3 && w.len() < 64)
                        .map(move |w| (w, i))
                })
                .collect();
            pairs.par_sort_unstable();
            pairs.dedup();
            let mut v = Vocab::default();
            v.postings.reserve_exact(pairs.len());
            let mut k = 0;
            while k < pairs.len() {
                let word = pairs[k].0.clone();
                let post_off = v.postings.len() as u32;
                while k < pairs.len() && pairs[k].0 == word {
                    v.postings.push(pairs[k].1);
                    k += 1;
                }
                let off = v.text.len() as u32;
                v.text.push_str(&word);
                v.words.push(VocabWord {
                    off,
                    len: word.len() as u16,
                    chars: word.chars().count().min(255) as u8,
                    bag: bag(word.as_bytes()),
                    post_off,
                    post_len: v.postings.len() as u32 - post_off,
                });
            }
            v.text.shrink_to_fit();
            v.words.shrink_to_fit();
            v
        })
    }
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
    /// Only folders.
    pub dirs_only: bool,
    /// Roots whose entries are hidden (unmounted volumes).
    pub offline_roots: &'a [u32],
}

/// Case-insensitive "starts with" without allocating.
fn starts_with_ci(name: &str, lower_prefix: &str) -> bool {
    let mut it = name.chars().flat_map(char::to_lowercase);
    lower_prefix.chars().all(|c| it.next() == Some(c))
}

/// `.bashrc`, `.secret`: a hidden name starting with the dot and `ext`.
fn dotfile_prefix(name: &[u8], ext: &str) -> bool {
    name.len() > ext.len()
        && name[0] == b'.'
        && name[1..=ext.len()].eq_ignore_ascii_case(ext.as_bytes())
}

/// Whether a node passes the non-text filters of `q`.
fn passes_filters(ix: &Index, i: u32, q: &Query) -> bool {
    let node = &ix.nodes[i as usize];
    match q.kind {
        Some(KindFilter::Dirs) if !node.is_dir() => return false,
        Some(KindFilter::Files) if node.is_dir() => return false,
        _ => {}
    }
    if !q.exts.is_empty() {
        let name = ix.name(i);
        let ext = extension(name);
        let ok = q.exts.iter().any(|e| {
            ext.is_some_and(|x| x.eq_ignore_ascii_case(e.as_bytes())) || dotfile_prefix(name, e)
        });
        if !ok {
            return false;
        }
    }
    if q.mtime_from.is_some() || q.mtime_to.is_some() {
        let m = crate::unpack_mtime(node.mtime);
        if q.mtime_from.is_some_and(|f| m < f) || q.mtime_to.is_some_and(|t| m >= t) {
            return false;
        }
    }
    if q.size_min.is_some() || q.size_max.is_some() {
        // Folder sizes are not known: size filters mean files.
        if node.is_dir()
            || q.size_min.is_some_and(|m| node.size < m)
            || q.size_max.is_some_and(|m| node.size > m)
        {
            return false;
        }
    }
    true
}

/// Search `ix` with a query in the syntax of [`crate::query`], parsed at
/// `ctx.now` (or the current time) in the local time zone. Returns at most
/// `limit` hits, best first.
pub fn search(
    ix: &Index,
    sd: &SearchData,
    query: &str,
    limit: usize,
    w: &Weights,
    ctx: &Context,
) -> Vec<Hit> {
    let now = jiff::Timestamp::from_second(ctx.now)
        .ok()
        .filter(|_| ctx.now > 0)
        .unwrap_or_else(jiff::Timestamp::now)
        .to_zoned_datetime(jiff::tz::TimeZone::system());
    search_query(ix, sd, &parse(query, &now), limit, w, ctx)
}

/// Search `ix` with a parsed query.
pub fn search_query(
    ix: &Index,
    sd: &SearchData,
    q: &Query,
    limit: usize,
    w: &Weights,
    ctx: &Context,
) -> Vec<Hit> {
    let terms = &q.terms[..q.terms.len().min(15)];
    let tokens: Vec<String> = terms.iter().map(|t| t.text.clone()).collect();
    if (tokens.is_empty() && !q.has_filters()) || ix.nodes.is_empty() {
        return Vec::new();
    }
    // Bits of tokens matched by the item's own name (vs. `in:` tokens,
    // matched by a folder above it only).
    let own_mask: u16 = terms
        .iter()
        .enumerate()
        .filter(|(_, t)| !t.ancestor_only)
        .fold(0, |m, (k, _)| m | 1 << k);
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
    let has_filters = q.has_filters();

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
                    // Nodes appended since the search data was built have
                    // no bag yet: let them through the prefilter.
                    let nb = sd.bags.get(i as usize).copied().unwrap_or(u64::MAX);
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
                        if let Some(alts) = sd.alt.get(i) {
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
        let vocab = sd.vocab(ix);
        let close: Vec<&VocabWord> = vocab
            .words
            .par_iter()
            .filter(|v| {
                usize::from(v.chars).abs_diff(len) <= max
                    && (v.bag ^ tbag).count_ones() as usize <= 2 * max
                    && vocab.word(v) != tok
                    && osa_distance(vocab.word(v), tok, max) <= max
            })
            .collect();
        for v in close {
            {
                let nodes =
                    &vocab.postings[v.post_off as usize..(v.post_off + v.post_len) as usize];
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
            let own = masks[i] & own_mask;
            let node = &ix.nodes[i];
            if own_mask != 0 {
                if own == 0 {
                    return None;
                }
            } else if node.is_dead() || node.parent == NONE {
                return None;
            }
            if ctx.dirs_only && !node.is_dir() {
                return None;
            }
            if has_filters && !passes_filters(ix, i as u32, q) {
                return None;
            }
            if !want_hidden && node.flags & flags::HIDDEN != 0 {
                // A dotfile asked for by its name (`.bashrc`).
                let asked = q.exts.iter().any(|e| dotfile_prefix(ix.name(i as u32), e));
                if !asked {
                    return None;
                }
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
            let mut root = i as u32;
            while p != NONE {
                root = p;
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
            if cum != all || ctx.offline_roots.contains(&root) {
                return None;
            }
            let own_score = if own == 0 { 0.0 } else { scores[i] };
            let mut score = own_score + anc * w.ancestor - depth as f32 * w.depth;
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
        for (k, atom) in atoms.iter().enumerate() {
            if own_mask & 1 << k == 0 {
                continue;
            }
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
    fn offline_roots_are_hidden() {
        let mut ix = build(&["a/report.pdf"]);
        let card = ix.add_root(Path::new("/media/card"), 0);
        ix.push(
            card,
            &Entry {
                name: b"report-card.pdf".to_vec(),
                kind: Kind::File,
                size: 1,
                mtime: 0,
            },
            0,
        );
        let sd = SearchData::build(&ix);
        let w = Weights::default();
        let all = search(&ix, &sd, "report", 10, &w, &Context::default());
        assert_eq!(all.len(), 2);
        let ctx = Context {
            offline_roots: &[card],
            ..Default::default()
        };
        let online = search(&ix, &sd, "report", 10, &w, &ctx);
        assert_eq!(online.len(), 1);
        assert_eq!(ix.path(online[0].node), Path::new("/r/a/report.pdf"));
    }

    /// A tree with mtimes and sizes, and a query parsed on Wednesday
    /// 2026-11-18 (UTC+8).
    fn filtered(paths: &[(&str, &str, u64)], query: &str) -> Vec<String> {
        let tz = jiff::tz::TimeZone::fixed(jiff::tz::offset(8));
        let day = |d: &str| {
            let d: jiff::civil::Date = d.parse().unwrap();
            d.at(12, 0, 0, 0)
                .to_zoned_datetime(tz.clone())
                .unwrap()
                .timestamp()
                .as_second()
        };
        let mut ix = Index::new();
        let root = ix.add_root(Path::new("/r"), 0);
        for (p, date, size) in paths {
            let comps: Vec<&str> = p.split('/').collect();
            let mut cur = root;
            for (k, c) in comps.iter().enumerate() {
                let last = k + 1 == comps.len();
                let found = ix.children(cur).find(|&x| ix.name(x) == c.as_bytes());
                cur = match found {
                    Some(x) => x,
                    None => ix.push(
                        cur,
                        &Entry {
                            name: c.as_bytes().to_vec(),
                            kind: if last && !p.ends_with('/') {
                                Kind::File
                            } else {
                                Kind::Dir
                            },
                            size: if last { *size } else { 0 },
                            mtime: day(date),
                        },
                        0,
                    ),
                };
            }
        }
        let sd = SearchData::build(&ix);
        let now = jiff::civil::date(2026, 11, 18)
            .at(15, 30, 0, 0)
            .to_zoned_datetime(tz.clone())
            .unwrap();
        let q = parse(query, &now);
        let mut out: Vec<String> =
            search_query(&ix, &sd, &q, 50, &Weights::default(), &Context::default())
                .iter()
                .map(|h| ix.path(h.node).display().to_string())
                .collect();
        out.sort();
        out
    }

    const FILES: &[(&str, &str, u64)] = &[
        ("work/合同/合同-终版.pdf", "2026-10-12", 300_000),
        ("work/合同/合同-草稿.docx", "2026-10-03", 20_000),
        ("work/合同/旧合同.pdf", "2026-09-28", 250_000),
        ("work/合同扫描.pdf", "2026-11-02", 9_000_000),
        ("work/报价.pdf", "2026-10-20", 50_000),
        ("acme/reports/q3.pdf", "2026-11-17", 1_000),
        ("acme/reports/q3.xlsx", "2026-11-18", 30_000_000),
        ("photos/big.mov", "2025-06-10", 4_000_000_000),
        (".bashrc", "2025-01-01", 100),
    ];

    #[test]
    fn acceptance_contract_pdf_last_month() {
        // PLAN M8: only PDFs modified last calendar month whose path
        // matches 合同 (the folder or the name).
        assert_eq!(
            filtered(FILES, "合同 .pdf 上月"),
            ["/r/work/合同/合同-终版.pdf"]
        );
    }

    #[test]
    fn filters() {
        assert_eq!(filtered(FILES, ".pdf in:acme"), ["/r/acme/reports/q3.pdf"]);
        assert_eq!(
            filtered(FILES, "q3 /reports/ yesterday"),
            ["/r/acme/reports/q3.pdf"]
        );
        assert_eq!(filtered(FILES, "today"), ["/r/acme/reports/q3.xlsx"]);
        assert_eq!(
            filtered(FILES, "size>5M"),
            [
                "/r/acme/reports/q3.xlsx",
                "/r/photos/big.mov",
                "/r/work/合同扫描.pdf"
            ]
        );
        assert_eq!(
            filtered(FILES, "size>5M <1y .pdf"),
            ["/r/work/合同扫描.pdf"]
        );
        // The folder took the date of its first file.
        assert_eq!(
            filtered(FILES, "date:2025-06"),
            ["/r/photos", "/r/photos/big.mov"]
        );
        assert_eq!(filtered(FILES, "dir: 合同"), ["/r/work/合同"]);
        assert_eq!(filtered(FILES, ".bashrc"), ["/r/.bashrc"]);
        // `in:` matches folders above the item only.
        assert_eq!(
            filtered(FILES, "in:合同 .pdf"),
            ["/r/work/合同/合同-终版.pdf", "/r/work/合同/旧合同.pdf"]
        );
        // An invalid filter is ignored; the rest still works.
        assert_eq!(filtered(FILES, "报价 <7x"), ["/r/work/报价.pdf"]);
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
