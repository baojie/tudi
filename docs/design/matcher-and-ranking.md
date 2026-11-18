# Matcher and ranking (M6)

Code: `crates/tudi-index/src/matcher.rs`; golden set:
`crates/tudi-index/tests/golden.rs`. Plan: `docs/PLAN.md` §5.5, ADR-013.

## Query

`query::parse` turns the text into fragments and filters
(`docs/query-syntax.md`); unreadable filters become `Hint`s instead of
errors. Fragments (≤ 15) are lowercased and order-free. A node is a hit
when every fragment matched its own name or an ancestor's, and at least
one plain fragment matched its own name; `in:` fragments count only on
ancestors. Hidden entries only when a fragment starts with a dot, an
extension filter names the dotfile, or `show_hidden`.

Filters (extension, kind, mtime range, size) are checked per node before
the ancestor walk, so they cost almost nothing. A filter-only query visits
every live node (≈ 4 ms on 1.85M entries). `Context::scope` keeps only
descendants of a folder (checked during the same walk), `offline_roots`
drops entries of unmounted roots.

## Per query

1. **Own-name matches** (parallel, chunks of 8k nodes): a 64-bit
   character bag per name rejects nodes that lack a token's characters;
   survivors are scored with `nucleo-matcher` fuzzy (subsequence) scoring,
   also against pinyin alternates of CJK names (full and initials, ×0.9).
   Per node: a token bitmask (`u16`) and a summed score, normalized by token
   length, +12 when the name starts with the token.
2. **Typo fallback**: a token of ≥ 4 alphanumerics with fewer than 5
   own-name matches also matches names containing a word within
   Damerau-Levenshtein 1 (2 for ≥ 8 characters). The vocabulary (words of
   all names with their nodes, in one arena) is built once in the
   background; candidate words are prefiltered by length and character-bag
   difference. Typo matches score 0.45 of a match.
3. **Ancestor walk** (parallel): each node with an own match walks up its
   parents to complete the token mask, sum ancestor scores (×0.35), count
   depth (−0.8 per level) and find the nearest frecent folder.
4. **Rank**: `(own + ancestors − depth + dir bonus) × (1 + frecency)` +
   a recency bonus decaying over about a month. Top K by
   `select_nth_unstable`, then sorted; match positions for the shown hits.

Weights live in `Weights`; frecency comes from the history database
(`Context::frecency`, normalized 0..=1 by folder node).

## Performance (1.83M entries)

p50 7.6 ms, p95 23 ms (P7: < 30 ms); single-character queries are the
slowest because almost every name passes the prefilter. See
`docs/benchmarks.md`.

## Quality

The golden set has 47 cases (English, Chinese, pinyin initials and full
pinyin, folder + name fragments in any order, misspellings): all are in the
top 3.
