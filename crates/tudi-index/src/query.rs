//! Query syntax (PLAN §5.5, M8; `docs/query-syntax.md`).
//!
//! Whitespace-separated tokens, all must hold (AND), in any order:
//!
//! | Form | Meaning |
//! |---|---|
//! | `report` | fuzzy fragment of the name or of a folder above it |
//! | `"two words"` | one fragment with a space |
//! | `in:acme`, `/acme/` | a folder above the item matches `acme` |
//! | `.pdf`, `ext:pdf,docx` | extension; `.bashrc` also finds dotfiles named so |
//! | `dir:`, `folder:`, `文件夹:` / `file:`, `文件:` | only folders / files |
//! | `<7d`, `>2w` | modified within / longer ago than (h d w mo y) |
//! | `>2025-06`, `<2025-06-15` | modified on/after / before a date |
//! | `date:2025-06` | modified in that year, month or day |
//! | `today` `yesterday` `week` `lastweek` `month` `lastmonth` `year` `lastyear` | calendar ranges |
//! | `今天` `昨天` `本周` `上周` `本月` `上月` `今年` `去年` | the same in Chinese |
//! | `size>10M`, `size<500k` | size (B, K, M, G, T; decimal) |
//!
//! Invalid filters do not fail the query: they are ignored and a short
//! hint is returned for the UI to show next to the results.

use jiff::civil::Date;
use jiff::{Span, ZonedDateTime};

/// A fuzzy fragment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Term {
    /// Lowercase.
    pub text: String,
    /// Must match a folder above the item, not the item itself (`in:`).
    pub ancestor_only: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KindFilter {
    Dirs,
    Files,
}

/// A problem with one token of a query, shown next to the results; the
/// token is ignored. Variants carry the token as typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hint {
    /// `in:` without a folder name.
    InNeedsName,
    /// `ext:` without an extension.
    ExtNeedsValue,
    /// `date:…` that is not a date.
    BadDate(String),
    /// `<7m`: months are `mo`.
    MonthIsMo(String),
    /// `<7`: no unit.
    AgeNeedsUnit(String),
    /// `<7x`, `<abc`.
    BadAge(String),
    /// `size>abc`, `size>5Q`, `size=5`.
    BadSize(String),
}

impl std::fmt::Display for Hint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Hint::InNeedsName => write!(f, "“in:” needs a folder name, e.g. in:acme"),
            Hint::ExtNeedsValue => write!(f, "“ext:” needs an extension, e.g. ext:pdf"),
            Hint::BadDate(t) => write!(f, "“{t}”: dates look like 2025, 2025-06 or 2025-06-15"),
            Hint::MonthIsMo(t) => {
                write!(f, "“{t}”: months are “mo” (<3mo); for sizes use size>10M")
            }
            Hint::AgeNeedsUnit(t) => write!(f, "“{t}”: add a unit: h, d, w, mo or y"),
            Hint::BadAge(t) => write!(f, "“{t}”: write <7d, >2w or >2025-06"),
            Hint::BadSize(t) => write!(f, "“{t}”: write size>10M or size<500k"),
        }
    }
}

/// A parsed query.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    pub terms: Vec<Term>,
    /// Lowercase extensions without the dot.
    pub exts: Vec<String>,
    pub kind: Option<KindFilter>,
    /// Modified in `[from, to)`, Unix seconds.
    pub mtime_from: Option<i64>,
    pub mtime_to: Option<i64>,
    pub size_min: Option<u64>,
    pub size_max: Option<u64>,
    /// Problems with filters, for an inline hint.
    pub hints: Vec<Hint>,
}

impl Query {
    /// No fragments and no filters.
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
            && self.exts.is_empty()
            && self.kind.is_none()
            && self.mtime_from.is_none()
            && self.mtime_to.is_none()
            && self.size_min.is_none()
            && self.size_max.is_none()
    }

    pub fn has_filters(&self) -> bool {
        !self.exts.is_empty()
            || self.kind.is_some()
            || self.mtime_from.is_some()
            || self.mtime_to.is_some()
            || self.size_min.is_some()
            || self.size_max.is_some()
    }

    fn from(&mut self, t: i64) {
        self.mtime_from = Some(self.mtime_from.map_or(t, |f| f.max(t)));
    }

    fn to(&mut self, t: i64) {
        self.mtime_to = Some(self.mtime_to.map_or(t, |f| f.min(t)));
    }
}

/// Split on whitespace, keeping `"quoted parts"` together.
fn tokenize(text: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut was_quoted = false;
    for c in text.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                was_quoted = true;
            }
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push((std::mem::take(&mut cur), was_quoted));
                }
                was_quoted = false;
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push((cur, was_quoted));
    }
    out
}

/// Start of day `d` in the time zone of `now` (DST-aware).
fn midnight(now: &ZonedDateTime, d: Date) -> i64 {
    d.to_zoned_datetime(now.time_zone().clone())
        .map_or(0, |z| z.timestamp().as_second())
}

fn ymd(y: i32, m: u32, d: u32) -> Option<Date> {
    Date::new(
        i16::try_from(y).ok()?,
        i8::try_from(m).ok()?,
        i8::try_from(d).ok()?,
    )
    .ok()
}

fn add_days(d: Date, n: i64) -> Option<Date> {
    d.checked_add(Span::new().try_days(n).ok()?).ok()
}

fn month_start(y: i32, m: u32) -> Option<Date> {
    ymd(y, m, 1)
}

fn prev_month(y: i32, m: u32) -> (i32, u32) {
    if m == 1 { (y - 1, 12) } else { (y, m - 1) }
}

fn next_month(y: i32, m: u32) -> (i32, u32) {
    if m == 12 { (y + 1, 1) } else { (y, m + 1) }
}

/// A calendar range by name: `[from, to)`.
fn named_range(word: &str, now: &ZonedDateTime) -> Option<(i64, Option<i64>)> {
    let today = now.date();
    let monday = add_days(today, -i64::from(today.weekday().to_monday_zero_offset()))?;
    let (y, m) = (i32::from(today.year()), today.month() as u32);
    Some(match word {
        "today" | "今天" => (midnight(now, today), None),
        "yesterday" | "昨天" => (
            midnight(now, add_days(today, -1)?),
            Some(midnight(now, today)),
        ),
        "week" | "thisweek" | "本周" => (midnight(now, monday), None),
        "lastweek" | "上周" => (
            midnight(now, add_days(monday, -7)?),
            Some(midnight(now, monday)),
        ),
        "month" | "thismonth" | "本月" => (midnight(now, month_start(y, m)?), None),
        "lastmonth" | "上月" => {
            let (py, pm) = prev_month(y, m);
            (
                midnight(now, month_start(py, pm)?),
                Some(midnight(now, month_start(y, m)?)),
            )
        }
        "year" | "thisyear" | "今年" => (midnight(now, ymd(y, 1, 1)?), None),
        "lastyear" | "去年" => (
            midnight(now, ymd(y - 1, 1, 1)?),
            Some(midnight(now, ymd(y, 1, 1)?)),
        ),
        _ => return None,
    })
}

/// `YYYY`, `YYYY-MM` or `YYYY-MM-DD`: (start, end) dates.
fn parse_date(s: &str) -> Option<(Date, Date)> {
    let parts: Vec<&str> = s.split('-').collect();
    let y: i32 = parts
        .first()?
        .parse()
        .ok()
        .filter(|y| (1970..=9999).contains(y))?;
    if parts[0].len() != 4 {
        return None;
    }
    match parts.len() {
        1 => Some((ymd(y, 1, 1)?, ymd(y + 1, 1, 1)?)),
        2 => {
            let m: u32 = parts[1].parse().ok()?;
            let (ny, nm) = next_month(y, m);
            Some((month_start(y, m)?, month_start(ny, nm)?))
        }
        3 => {
            let d = ymd(y, parts[1].parse().ok()?, parts[2].parse().ok()?)?;
            Some((d, add_days(d, 1)?))
        }
        _ => None,
    }
}

/// `7d`, `12h`, `2w`, `3mo`, `1y` → seconds; `tok` is the whole token.
fn parse_age(s: &str, tok: &str) -> Result<i64, Hint> {
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: i64 = num.parse().map_err(|_| Hint::BadAge(tok.into()))?;
    let secs = match unit {
        "h" => 3_600,
        "d" => 86_400,
        "w" => 7 * 86_400,
        "mo" => 30 * 86_400,
        "y" => 365 * 86_400,
        "m" => return Err(Hint::MonthIsMo(tok.into())),
        "" => return Err(Hint::AgeNeedsUnit(tok.into())),
        _ => return Err(Hint::BadAge(tok.into())),
    };
    Ok(n.saturating_mul(secs))
}

/// `10M`, `1.5G`, `500k`, `200` → bytes (decimal units).
fn parse_size(s: &str) -> Option<u64> {
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: f64 = num.parse().ok()?;
    let mult = match unit.to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "k" | "kb" => 1e3,
        "m" | "mb" => 1e6,
        "g" | "gb" => 1e9,
        "t" | "tb" => 1e12,
        _ => return None,
    };
    Some((n * mult) as u64)
}

fn is_ext(s: &str) -> bool {
    (1..=8).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Parse `text` now, in the system time zone.
pub fn parse_local(text: &str) -> Query {
    parse(
        text,
        &jiff::Timestamp::now().to_zoned_datetime(jiff::tz::TimeZone::system()),
    )
}

/// Parse `text` at time `now` (local offset).
pub fn parse(text: &str, now: &ZonedDateTime) -> Query {
    let mut q = Query::default();
    for (tok, quoted) in tokenize(text) {
        if quoted {
            q.terms.push(Term {
                text: tok.to_lowercase(),
                ancestor_only: false,
            });
            continue;
        }
        let lower = tok.to_lowercase();
        let t = lower.as_str();
        // Calendar words.
        if let Some((from, to)) = named_range(t, now) {
            q.from(from);
            if let Some(to) = to {
                q.to(to);
            }
            continue;
        }
        // Kind (optionally followed by a fragment: `dir:acme`).
        let kinds: [(&str, KindFilter); 5] = [
            ("dir:", KindFilter::Dirs),
            ("folder:", KindFilter::Dirs),
            ("文件夹:", KindFilter::Dirs),
            ("file:", KindFilter::Files),
            ("文件:", KindFilter::Files),
        ];
        if let Some((rest, k)) = kinds
            .iter()
            .find_map(|(p, k)| t.strip_prefix(p).map(|r| (r, *k)))
        {
            q.kind = Some(k);
            if !rest.is_empty() {
                q.terms.push(Term {
                    text: rest.to_string(),
                    ancestor_only: false,
                });
            }
            continue;
        }
        if let Some(rest) = t.strip_prefix("in:") {
            if rest.is_empty() {
                q.hints.push(Hint::InNeedsName);
            } else {
                q.terms.push(Term {
                    text: rest.to_string(),
                    ancestor_only: true,
                });
            }
            continue;
        }
        if t.len() > 2 && t.starts_with('/') && t.ends_with('/') {
            q.terms.push(Term {
                text: t[1..t.len() - 1].to_string(),
                ancestor_only: true,
            });
            continue;
        }
        if let Some(rest) = t.strip_prefix("ext:") {
            let exts: Vec<String> = rest
                .split(',')
                .map(|e| e.trim_start_matches('.').to_string())
                .filter(|e| !e.is_empty())
                .collect();
            if exts.is_empty() {
                q.hints.push(Hint::ExtNeedsValue);
            }
            q.exts.extend(exts);
            continue;
        }
        if let Some(rest) = t.strip_prefix('.')
            && is_ext(rest)
        {
            q.exts.push(rest.to_string());
            continue;
        }
        if let Some(rest) = t.strip_prefix("date:") {
            match parse_date(rest) {
                Some((a, b)) => {
                    q.from(midnight(now, a));
                    q.to(midnight(now, b));
                }
                None => q.hints.push(Hint::BadDate(tok.clone())),
            }
            continue;
        }
        // `size>10M`, `size:<5k`; a word like `sizes` is a fragment.
        if let Some(rest) = t
            .strip_prefix("size")
            .filter(|r| r.starts_with([':', '<', '>', '=']))
        {
            let rest = rest.strip_prefix(':').unwrap_or(rest);
            let (op, val) = match rest.chars().next() {
                Some('>') => ('>', &rest[1..]),
                Some('<') => ('<', &rest[1..]),
                _ => {
                    q.hints.push(Hint::BadSize(tok.clone()));
                    continue;
                }
            };
            let val = val.strip_prefix('=').unwrap_or(val);
            match parse_size(val) {
                Some(b) if op == '>' => q.size_min = Some(b),
                Some(b) => q.size_max = Some(b),
                None => q.hints.push(Hint::BadSize(tok.clone())),
            }
            continue;
        }
        if let Some(op) = t.chars().next().filter(|c| *c == '<' || *c == '>') {
            let val = &t[1..];
            let val = val.strip_prefix('=').unwrap_or(val);
            if let Some((a, _)) = parse_date(val) {
                // >DATE: on or after it; <DATE: before it.
                let at = midnight(now, a);
                if op == '>' { q.from(at) } else { q.to(at) }
            } else {
                match parse_age(val, &tok) {
                    Ok(secs) => {
                        let at = now.timestamp().as_second() - secs;
                        // <7d: newer than 7 days; >7d: older.
                        if op == '<' { q.from(at) } else { q.to(at) }
                    }
                    Err(e) => q.hints.push(e),
                }
            }
            continue;
        }
        q.terms.push(Term {
            text: lower,
            ancestor_only: false,
        });
    }
    q
}

/// Lowercase extension of a name (after the last dot, not a leading one).
pub fn extension(name: &[u8]) -> Option<&[u8]> {
    let p = name.iter().rposition(|&c| c == b'.')?;
    (p > 0 && p + 1 < name.len()).then(|| &name[p + 1..])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wednesday 2026-11-18 15:30 at UTC+8.
    fn zoned(hours: i8, y: i16, m: i8, d: i8, h: i8) -> ZonedDateTime {
        jiff::civil::date(y, m, d)
            .at(h, 0, 0, 0)
            .to_zoned_datetime(jiff::tz::TimeZone::fixed(jiff::tz::offset(hours)))
            .unwrap()
    }

    fn now() -> ZonedDateTime {
        let n = zoned(8, 2026, 11, 18, 15);
        n.checked_add(Span::new().minutes(30)).unwrap()
    }

    fn at(y: i16, m: i8, d: i8) -> i64 {
        zoned(8, y, m, d, 0).timestamp().as_second()
    }

    fn terms(q: &Query) -> Vec<(&str, bool)> {
        q.terms
            .iter()
            .map(|t| (t.text.as_str(), t.ancestor_only))
            .collect()
    }

    #[test]
    fn plain_and_quoted() {
        let q = parse("Report  \"annual plan\" 合同", &now());
        assert_eq!(
            terms(&q),
            [("report", false), ("annual plan", false), ("合同", false)]
        );
        assert!(!q.has_filters() && q.hints.is_empty());
        assert!(parse("   ", &now()).is_empty());
    }

    #[test]
    fn extensions_and_kinds() {
        let q = parse(".PDF ext:docx,.xlsx dir:acme", &now());
        assert_eq!(q.exts, ["pdf", "docx", "xlsx"]);
        assert_eq!(q.kind, Some(KindFilter::Dirs));
        assert_eq!(terms(&q), [("acme", false)]);
        assert_eq!(parse("file:", &now()).kind, Some(KindFilter::Files));
        assert_eq!(parse("文件夹:", &now()).kind, Some(KindFilter::Dirs));
        // Not an extension: too long or with other characters.
        assert_eq!(
            terms(&parse(".config-old", &now())),
            [(".config-old", false)]
        );
    }

    #[test]
    fn folders() {
        let q = parse("in:acme /reports/ plan", &now());
        assert_eq!(
            terms(&q),
            [("acme", true), ("reports", true), ("plan", false)]
        );
        assert!(!parse("in:", &now()).hints.is_empty());
    }

    #[test]
    fn calendar_words() {
        let n = now(); // Wednesday 2026-11-18
        let q = parse("today", &n);
        assert_eq!((q.mtime_from, q.mtime_to), (Some(at(2026, 11, 18)), None));
        let q = parse("昨天", &n);
        assert_eq!(
            (q.mtime_from, q.mtime_to),
            (Some(at(2026, 11, 17)), Some(at(2026, 11, 18)))
        );
        let q = parse("本周", &n);
        assert_eq!(q.mtime_from, Some(at(2026, 11, 16)));
        let q = parse("lastweek", &n);
        assert_eq!(
            (q.mtime_from, q.mtime_to),
            (Some(at(2026, 11, 9)), Some(at(2026, 11, 16)))
        );
        let q = parse("上月", &n);
        assert_eq!(
            (q.mtime_from, q.mtime_to),
            (Some(at(2026, 10, 1)), Some(at(2026, 11, 1)))
        );
        let q = parse("month", &n);
        assert_eq!(q.mtime_from, Some(at(2026, 11, 1)));
        let q = parse("去年", &n);
        assert_eq!(
            (q.mtime_from, q.mtime_to),
            (Some(at(2025, 1, 1)), Some(at(2026, 1, 1)))
        );
        // January: last month is December of the year before.
        let jan = zoned(0, 2027, 1, 5, 0);
        let q = parse("lastmonth", &jan);
        let utc = |y, m, d| zoned(0, y, m, d, 0).timestamp().as_second();
        assert_eq!(
            (q.mtime_from, q.mtime_to),
            (Some(utc(2026, 12, 1)), Some(utc(2027, 1, 1)))
        );
    }

    #[test]
    fn relative_and_dates() {
        let n = now();
        let q = parse("<7d", &n);
        assert_eq!(q.mtime_from, Some(n.timestamp().as_second() - 7 * 86_400));
        let q = parse(">2w", &n);
        assert_eq!(q.mtime_to, Some(n.timestamp().as_second() - 14 * 86_400));
        let q = parse(">2025-06", &n);
        assert_eq!(q.mtime_from, Some(at(2025, 6, 1)));
        let q = parse("<2025-06-15", &n);
        assert_eq!(q.mtime_to, Some(at(2025, 6, 15)));
        let q = parse("date:2025-06", &n);
        assert_eq!(
            (q.mtime_from, q.mtime_to),
            (Some(at(2025, 6, 1)), Some(at(2025, 7, 1)))
        );
        let q = parse("date:2024", &n);
        assert_eq!(
            (q.mtime_from, q.mtime_to),
            (Some(at(2024, 1, 1)), Some(at(2025, 1, 1)))
        );
        // Several time filters intersect.
        let q = parse("<30d >1w", &n);
        assert!(q.mtime_from.unwrap() < q.mtime_to.unwrap());
    }

    #[test]
    fn sizes() {
        let q = parse("size>10M size<1.5G", &now());
        assert_eq!(
            (q.size_min, q.size_max),
            (Some(10_000_000), Some(1_500_000_000))
        );
        assert_eq!(parse("size:>500k", &now()).size_min, Some(500_000));
        assert_eq!(parse("size<200", &now()).size_max, Some(200));
    }

    #[test]
    fn invalid_filters_give_hints_not_errors() {
        for bad in [
            "<7m",
            "<7",
            "<7x",
            "size>abc",
            "size=5",
            "size>5Q",
            "date:2025-13",
            "ext:",
        ] {
            let q = parse(&format!("plan {bad}"), &now());
            assert_eq!(
                terms(&q),
                [("plan", false)],
                "{bad}: other tokens still work"
            );
            assert_eq!(q.hints.len(), 1, "{bad}: one hint");
            assert!(!q.has_filters() || bad.starts_with("ext"), "{bad}");
        }
        assert_eq!(parse("<7m", &now()).hints, [Hint::MonthIsMo("<7m".into())]);
        assert!(parse("<7m", &now()).hints[0].to_string().contains("mo"));
        // Words that only start like a filter are fragments.
        let q = parse("sizes sizeof.h", &now());
        assert_eq!(terms(&q), [("sizes", false), ("sizeof.h", false)]);
        assert!(q.hints.is_empty());
    }

    #[test]
    fn extension_helper() {
        assert_eq!(extension(b"a.PDF"), Some(&b"PDF"[..]));
        assert_eq!(extension(b".bashrc"), None);
        assert_eq!(extension(b"a.tar.gz"), Some(&b"gz"[..]));
        assert_eq!(extension(b"noext"), None);
        assert_eq!(extension(b"trailing."), None);
    }
}
