//! Natural, case-insensitive ordering of file names (PLAN §5.1).
//!
//! Rules, in order:
//! 1. One leading `.` is ignored, so `.bashrc` sorts among the `b`s.
//! 2. Runs of ASCII digits compare by numeric value (`file2` < `file10`),
//!    without parsing, so arbitrarily long runs work.
//! 3. Other bytes compare ASCII-case-insensitively. Non-ASCII bytes compare
//!    raw; for valid UTF-8 that is code-point order, which puts CJK after
//!    Latin and keeps CJK in code-point order (pinyin collation is M8).
//! 4. Ties (`a` vs `A`, `file01` vs `file1`, `.x` vs `x`) are broken by the
//!    raw bytes, so the order is total and deterministic.

use std::cmp::Ordering;

/// Total order used for listing names. See the module docs.
pub fn compare(a: &[u8], b: &[u8]) -> Ordering {
    primary(strip_dot(a), strip_dot(b)).then_with(|| a.cmp(b))
}

fn strip_dot(s: &[u8]) -> &[u8] {
    match s {
        [b'.', rest @ ..] if !rest.is_empty() => rest,
        _ => s,
    }
}

fn digit_run(s: &[u8], start: usize) -> usize {
    let mut end = start;
    while end < s.len() && s[end].is_ascii_digit() {
        end += 1;
    }
    end
}

fn trim_zeros(s: &[u8]) -> &[u8] {
    let n = s.iter().take_while(|&&c| c == b'0').count();
    &s[n..]
}

/// Natural, case-insensitive comparison without the raw-byte tiebreak.
fn primary(a: &[u8], b: &[u8]) -> Ordering {
    let (mut i, mut j) = (0, 0);
    loop {
        match (a.get(i), b.get(j)) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(&x), Some(&y)) => {
                if x.is_ascii_digit() && y.is_ascii_digit() {
                    let (ei, ej) = (digit_run(a, i), digit_run(b, j));
                    let (na, nb) = (trim_zeros(&a[i..ei]), trim_zeros(&b[j..ej]));
                    let ord = na.len().cmp(&nb.len()).then_with(|| na.cmp(nb));
                    if ord != Ordering::Equal {
                        return ord;
                    }
                    (i, j) = (ei, ej);
                } else {
                    let ord = x.to_ascii_lowercase().cmp(&y.to_ascii_lowercase());
                    if ord != Ordering::Equal {
                        return ord;
                    }
                    (i, j) = (i + 1, j + 1);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(names: &[&str]) -> Vec<String> {
        let mut v: Vec<&[u8]> = names.iter().map(|s| s.as_bytes()).collect();
        v.sort_by(|a, b| compare(a, b));
        v.into_iter()
            .map(|s| String::from_utf8(s.to_vec()).unwrap())
            .collect()
    }

    #[test]
    fn numbers_compare_by_value() {
        assert_eq!(
            sorted(&["file10", "file2", "file1", "file100", "file20"]),
            ["file1", "file2", "file10", "file20", "file100"]
        );
    }

    #[test]
    fn leading_zeros_tie_break_by_bytes() {
        assert_eq!(sorted(&["a1", "a01", "a001"]), ["a001", "a01", "a1"]);
        assert_eq!(
            sorted(&["v2.10", "v2.9", "v10.0"]),
            ["v2.9", "v2.10", "v10.0"]
        );
    }

    #[test]
    fn very_long_numbers() {
        assert_eq!(
            sorted(&["x123456789012345678901234567890", "x99"]),
            ["x99", "x123456789012345678901234567890"]
        );
    }

    #[test]
    fn case_insensitive_with_stable_tiebreak() {
        assert_eq!(
            sorted(&["banana", "Apple", "apple", "Cherry"]),
            ["Apple", "apple", "banana", "Cherry"]
        );
    }

    #[test]
    fn dotfiles_sort_among_their_letter() {
        assert_eq!(
            sorted(&["c", ".bashrc", "a", "bashrc", ".", ".."]),
            [".", "..", "a", ".bashrc", "bashrc", "c"]
        );
    }

    #[test]
    fn cjk_after_latin_in_code_point_order() {
        // 合 U+5408, 季 U+5B63, 报 U+62A5
        assert_eq!(
            sorted(&["报告", "季报", "zeta", "合同", "Alpha", "2024"]),
            ["2024", "Alpha", "zeta", "合同", "季报", "报告"]
        );
    }

    #[test]
    fn cjk_with_numbers() {
        assert_eq!(
            sorted(&["第10章", "第2章", "第1章"]),
            ["第1章", "第2章", "第10章"]
        );
    }

    #[test]
    fn non_utf8_is_ordered_without_panicking() {
        let mut v: Vec<&[u8]> = vec![b"caf\xe9", b"cafe", b"\xff\xfe", b"Cafe"];
        v.sort_by(|a, b| compare(a, b));
        assert_eq!(v, [&b"Cafe"[..], b"cafe", b"caf\xe9", b"\xff\xfe"]);
    }

    #[test]
    fn prefix_sorts_first() {
        assert_eq!(sorted(&["abc", "ab", "abcd"]), ["ab", "abc", "abcd"]);
        assert_eq!(sorted(&["a1b", "a1"]), ["a1", "a1b"]);
    }

    proptest::proptest! {
        /// `sort_by` requires a total order; check the laws on names drawn
        /// from an alphabet that stresses digits, dots, case and zeros.
        #[test]
        fn is_a_total_order(
            a in "[a0A1.9b]{0,6}",
            b in "[a0A1.9b]{0,6}",
            c in "[a0A1.9b]{0,6}",
        ) {
            let (a, b, c) = (a.as_bytes(), b.as_bytes(), c.as_bytes());
            proptest::prop_assert_eq!(compare(a, b), compare(b, a).reverse());
            proptest::prop_assert_eq!(compare(a, b) == Ordering::Equal, a == b);
            if compare(a, b) != Ordering::Greater && compare(b, c) != Ordering::Greater {
                proptest::prop_assert_ne!(compare(a, c), Ordering::Greater);
            }
        }
    }
}
